# Close the benchmark gaps: plan

Owner ask (2026-11-01): find every place AtticDB loses to SQLite, Redis and the rest, and win with purpose-built engineering. This plan lists the losses we measured, the cause of each, and 1 phase per cause. Every phase is an A/B on the bench VM. A change ships only if it wins without a regression.

## Where we lose today

Source: [H2H.md](../../bench/H2H.md) (Azure L8s_v3, 7 interleaved reps), #52, #64, #69, and the real-transcript run in [RESULTS.md](../../bench/RESULTS.md).

| # | Loss | Measured | Cause, as measured |
|---|---|---|---|
| L1 | Throughput past 32 writers, NVMe | 68,168 vs 79,957 to 80,971 acks/s at 128 writers (Redis family) | Thread per connection. Connection threads burn 35.9 µs of CPU per append, against 6.0 for the writer and 3.1 for replies. Each request costs 2 sleeps and 2 wakes (#64). The box is out of CPU |
| L2 | Tail at 16 writers, NVMe, closed loop | p99.9 674 vs 604 to 624 µs. Max 1,210 vs 810 to 947 µs | Same handoffs. At 64 and 128 writers, also each checkpoint copying the retry-key window under the state lock: 5 ms stalls (#69) |
| L3 | 1 writer, NVMe, vs SQLite | 9,799 vs 19,410 acks/s. p50 94 vs 32 µs | SQLite runs in process. We pay an HTTP round trip on a Unix socket and 2 thread handoffs. `fdatasync` alone is about 26 µs |
| L4 | Reads | Not benchmarked against anyone. Search p99 is 618 ms at 200k turns | `memory_search` scans the tape newest first, with no index (D86) |
| L5 | Agent-shaped p99 at 256 writers vs Valkey and DiceDB | 558 vs 348 to 496 µs (medians), ranges overlap | Mostly VM fast/slow modes that hit every system (#47, #48). Re-check after L1 |

We already win: acks/s and p99 on Premium SSD v2 at every writer count (1 device round trip per group, not 2). p99 against the Redis family at 16 writers on NVMe. And SQLite's multi-second stalls under concurrency. SQLite still has the lower p50 on both disks, and the lower p99 on NVMe at 4 and 16 writers.

## Rules for every phase

- 1 change per A/B. Keep it only if it wins and nothing regresses: acks/s and p99 at 16, 64 and 128 writers, on NVMe and Premium SSD v2.
- Interleaved reps with Redis as the control, as in `h2h.sh`. Report the ratio to Redis next to the absolute numbers.
- No new crates. No `unsafe` outside `atticdb-sys`. The tape format and the HTTP API don't change.
- Bench VM: `zdb-g1`. About 20 minutes and $0.20 to $0.25 per A/B session (#52). Deallocate after each session and report the cost.

## Phase 1: event-driven connections (L1, L2, L3)

Replace the thread per connection with a few I/O threads, on Linux and macOS. Windows keeps threads for now.

- **I/O threads:** 2 by default, at most `available_parallelism / 2`. Each owns its connections and blocks in `poll(2)` (`rustix::event::poll`, already used in `serve.rs`). With at most 256 connections, poll's O(n) scan is cheap. epoll comes later only if a profile shows poll. kqueue's rustix API is `unsafe`, so it's out.
- **Accept:** the accept threads hand each new socket to the I/O thread with the fewest connections. A `UnixStream::pair` per I/O thread is its wake fd.
- **Per-connection state machine:** `Head`, `Body`, `Pending`, `Write` and `Linger`. Each state keeps today's total deadline from `http.rs`: head, idle, body, write, linger. The poll timeout is the nearest deadline. `Expect: 100-continue` writes the interim response before `Body`, as now.
- **Writes:** the I/O thread parses and calls a new non-blocking engine submit. The writer's reply thread pushes each result to its owner's queue, then writes 1 byte to each owner's wake fd once per group. That's at most 2 wakes per group, not 2 per request.
- **Reads:** window, fact and search run on a small worker pool. A slow search must never block an I/O thread.
- **Unchanged:** MAX_CONNECTIONS (256), body and window budgets, `busy` replies, token auth, the `Host` check, keep-alive, `--log-requests`, and every response byte.

Done when, on NVMe at 128 writers:
- acks/s reach at least the Redis family median;
- connection-layer CPU per append is under 12 µs (`/proc` per-thread time);
- p99 at 16 writers is no worse, and Premium SSD v2 doesn't regress;
- the full `atticdb` suite and both sims pass.

## Phase 2: checkpoints off the state lock (L2)

As proposed in #69:
- Deltas take a publish log in O(1) under the lock.
- Full snapshots are built off the lock, from the last full file plus the deltas.

Done when server acks over 5 ms are near 0 at 64 and 128 writers, and a full snapshot of a 1M-key window holds the lock for under 2 ms on the bench VM.

## Phase 3: reads and search (L4)

1. **Benchmark first.** Extend `h2h.sh` with read workloads against SQLite and Redis:
   - last 20 turns of a session: SQLite indexed `SELECT`, Redis `LRANGE`;
   - fact get by key: SQLite primary key, Redis `GET`;
   - word search: SQLite FTS5.
   Use the `realload.py` corpus.
2. **Search index (#117, built as 0006).** A word index derived from the tape. It's rebuilt on open or from checkpoints, never the source of truth.

Done when search p99 is under 5 ms at 200k turns, and window and fact reads are at or under SQLite's p99.

## Phase 4: in-process writes (L3)

SQLite wins at 1 writer because it has no socket. `atticdb-core`'s `Engine` is already a Rust library. Add an "embedded" row to H2H: the engine in the writer's process, against SQLite in process. This compares like with like. It also shows how much of the 1-writer gap Phase 1 left in the socket path.

Done when embedded 1-writer p50 on NVMe is within 10% of SQLite's, which is about `fdatasync` time.

## Out of scope

- io_uring, async runtimes, and a thread-per-core rewrite. #47 and #64 ruled them out until a profile asks.
- PostgreSQL, Ultra Disk, and replication.
- GPU offload (D83).

## Order and cost

Phase 1, then 2, then 3, then 4. Phase 1 is the largest code change, and it changes every later number.

Expect 4 to 6 VM sessions, about $1 to $1.50 in total.
