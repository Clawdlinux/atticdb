# zerodb

Agent memory database written in Rust. Fast and crash safe first.

Status: early. Milestones 1 (the tape) and 2 (the session log engine) are done. Nothing here is ready for production use.
M2 Groups D and E add durable session history. G1 and engine simulation remain separate acceptance work.

## Why

Agents need memory that does not lose writes and does not slow down as history grows. Today teams glue Redis, a vector DB and a memory framework together. We keep seeing the same failures:

- writes that report success but never persist;
- "last N turns" that gets slower as a session grows;
- scoped search that leaks or misses records;
- two processes sharing one memory file and overwriting each other.

zerodb puts session history, hybrid recall and a cache behind one durable commit point.

## Design in one paragraph

One append-only, checksummed, per-tenant hash-chained log (the tape) is the write-ahead log, the data store and the provenance chain at once. Every other structure is a view rebuilt from the tape. Group commit keeps the durable ack close to a bare append plus sync. Full design: [docs/design/0001-architecture.md](docs/design/0001-architecture.md). Decision log: [docs/design/0002-decision-log.md](docs/design/0002-decision-log.md).

## What exists today (M1, M2)

- `crates/zerodb-tape`: frames, CRC32C, BLAKE3 tenant chains, segment files, manifest and superblock, a group-commit writer, and recovery.
- `SimDisk`: a deterministic fault-injecting disk. It loses, reorders and tears unsynced writes, fails syncs fsyncgate-style, flips bits, and runs out of space.
- A seeded simulation that crashes the writer at random points across 2 to 3 generations per seed and checks that no acked write is lost. 10,000 seeds pass.
- `crates/zerodb-core`: the session log engine. `append` and `window` with dense per-session seqs, read-your-writes, O(returned) windows, idempotent retries, tenants, and incremental checkpoints.
- An engine-level seeded simulation with grouped commits, crashes, live reopens and corrupt checkpoints. 10,000 seeds pass.
- Benchmarks against a bare append-plus-sync baseline: [bench/RESULTS.md](bench/RESULTS.md).

### Tape hooks (M2 Group D)

`Tape::open_with(state_machine, io, clock, path, options)` attaches application state to the tape.
`Tape::open(...)` keeps the tape-only behavior through `NoopStateMachine`.

Implement `StateMachine` to assign sequences during `prepare`, then install synced records during `publish`.
Preparation runs once per batch, in queue order, on the writer thread.
`PreparedBatch::records_mut()` allows content and metadata rewrites. Changed content gets fresh digests before framing.
`Prepare::Reject` and `Prepare::Reply` write nothing.
Keep preparation reservations private. Storage failures poison the writer, so later writes cannot observe those reservations.
Preparation may cache dedup entries for writes still pending in the current group.
Cached durable replies wait for sync and publication. Failed syncs return `Unknown` for originals and retries.
Fast requests never receive LSNs, including cached replies.

Publication runs before durable replies. Fast batches publish only after a successful sync, including clean shutdown.
Fast-only traffic forces sync after a segment roll or when buffered encoded bytes exceed a threshold.
`TapeOptions::max_unpublished_bytes` configures that threshold. `None` uses the segment size.
The threshold is checked at the end of each writer step. Successful syncs release rolled segment handles.
During open, recovery finishes and syncs before ordered `replay` callbacks start.
Set `TapeOptions::replay_from_lsn` to your loaded checkpoint's watermark. Only records strictly above it replay.
A replay error aborts open.

`Tape::scan(from_lsn)` yields durable `CommittedRecord` values in LSN order, starting inclusively.
The iterator captures its durable boundary and reads one indexed frame at a time across segments.
Footers and redacted husks are skipped. Application payloads exclude salts.

### Session engine (M2 Group E)

`crates/zerodb-core` provides `Engine::open`, `append`, and ordered `window` reads.
The writer assigns dense session sequences and publishes the index before acknowledging durable writes.
Retry keys detect conflicts and return original receipts within bounded per-tenant and global windows.
Geometric session pages use a global LRU budget. Cold pages reload from incremental checkpoint files.
Checkpoint I/O runs on a separate worker through the tape's `Io` trait.
Corrupt checkpoints trigger full replay with a warning.

See [the core API and storage notes](crates/zerodb-core/README.md) for formats, limits, and recovery behavior.

## Roadmap

| Milestone | Scope |
|---|---|
| M1 | Tape, I/O, recovery, simulation. Done. |
| M2 | Session index, append and window, idempotency, checkpoints. Done. G1 passes on macOS (1.40x median vs 1.5x gate). Linux NVMe run pending. |
| M3 | Cache with TTL, pub-sub, temporal facts. |
| M4 | Compaction, forget, retention, signed checkpoints, verify, export. |
| M5 | BM25, exact vector scan, RRF. Kill gate G2. |
| M6 | HTTP server with auth, MCP stdio adapter, Python SDK, markdown import and export. |
| M7 | PrefixLedger, prefix-cache-stable context assembly. Kill gate G3. |

## Build and test

```sh
cargo test --workspace
cargo test --workspace --release
ZERODB_SIM_SEEDS=10000 cargo test --release --features sim --test sim
```

## License

MIT OR Apache-2.0.
