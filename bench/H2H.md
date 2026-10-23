# Head-to-head: durable appends

Run on 2026-10-23. Raw rows: [h2h-nvme.tsv](h2h-nvme.tsv) and [h2h-premium-v2.tsv](h2h-premium-v2.tsv). Runbook: [h2h.sh](h2h.sh).

![Durable acks per second and p99 latency, by disk and writer count](h2h.svg)

Redraw with `python3 bench/h2h_chart.py`.

## The question

How fast can each system acknowledge a write that survives power loss? Agents write that way: one append, wait for the ack, next append. So every client here is closed-loop with one request in flight, and every system acks only after `fsync`.

## Setup

- Azure Standard_L8s_v3 (8 vCPU), Linux 7.0, ext4, a freshly formatted disk per pass.
- Two disks: the VM's local NVMe, and a Premium SSD v2 managed disk (128 GiB, 3,000 IOPS, 125 MB/s, host caching off).
- 1, 4 and 16 writers. 5,000 appends of 256 bytes per writer. A fresh data directory per run. Medians of 3 repetitions.

| System | Version | Durability setting | Workload | Client |
|---|---|---|---|---|
| zerodb | `ba9c4c3` (main + #23) | Always: group commit, ack after `fdatasync` | `POST /v1/append`, one session per writer | `zerodb load`, HTTP over the Unix socket |
| Redis | 8.10.2 | `appendonly yes`, `appendfsync always` | `RPUSH` to 64 keys | memtier_benchmark 2.5.2, Unix socket, pipeline 1 |
| Valkey | 9.1.2 | Same | Same | Same |
| DiceDB | 1.0.0 (Valkey 8.0.6 fork) | Same | Same | Same |
| SQLite | 3.45.1 | WAL, `synchronous=FULL`, one row per transaction | `INSERT` | [h2h_sqlite.py](h2h_sqlite.py), one process and connection per writer |

Dragonfly is not timed. It has no append-only file, so it cannot acknowledge a write after `fsync` ([docs](https://www.dragonflydb.io/docs/managing-dragonfly/aof)).

## Premium SSD v2

| Writers | System | Acks/s | p50 µs | p99 µs | p99.9 µs | max µs |
|---|---|---|---|---|---|---|
| 1 | zerodb | 1,255 | 755 | 1,515 | 1,792 | 2,246 |
| 1 | sqlite | 1,171 | 704 | 2,249 | 2,825 | 3,666 |
| 1 | redis | 458 | 2,159 | 3,103 | 3,583 | 3,855 |
| 1 | valkey | 458 | 2,159 | 3,055 | 3,567 | 4,319 |
| 1 | dicedb | 457 | 2,143 | 3,071 | 3,743 | 4,047 |
| 4 | zerodb | 2,652 | 1,427 | 2,248 | 2,890 | 4,337 |
| 4 | sqlite | 1,361 | 704 | 1,856 | 2,798 | 8,639,826 |
| 4 | redis | 938 | 4,287 | 5,471 | 6,367 | 9,599 |
| 4 | valkey | 936 | 4,255 | 5,471 | 6,111 | 6,655 |
| 4 | dicedb | 934 | 4,255 | 5,439 | 6,111 | 8,831 |
| 16 | zerodb | 15,480 | 1,053 | 2,456 | 3,716 | 13,409 |
| 16 | sqlite | 1,462 | 703 | 1,674 | 2,754 | 49,177,444 |
| 16 | valkey | 4,153 | 3,887 | 5,119 | 5,919 | 12,799 |
| 16 | dicedb | 4,102 | 3,903 | 5,151 | 6,815 | 10,047 |
| 16 | redis | 4,061 | 3,919 | 5,151 | 6,015 | 11,519 |

zerodb acknowledges 2.7× as many writes as Redis, Valkey and DiceDB at 1 writer, 2.8× at 4 and 3.7× to 3.8× at 16. Its p99 and p99.9 stay lower at every level. Its max at 16 writers is in the same range as theirs.

## Local NVMe

| Writers | System | Acks/s | p50 µs | p99 µs | p99.9 µs | max µs |
|---|---|---|---|---|---|---|
| 1 | sqlite | 20,154 | 31 | 144 | 380 | 718 |
| 1 | zerodb | 10,828 | 89 | 116 | 149 | 822 |
| 1 | dicedb | 9,184 | 111 | 167 | 199 | 391 |
| 1 | valkey | 9,182 | 111 | 167 | 191 | 343 |
| 1 | redis | 9,019 | 111 | 167 | 191 | 455 |
| 4 | sqlite | 24,901 | 31 | 131 | 689 | 629,587 |
| 4 | zerodb | 22,961 | 168 | 226 | 393 | 2,995 |
| 4 | dicedb | 20,522 | 191 | 255 | 327 | 455 |
| 4 | redis | 20,407 | 191 | 255 | 343 | 503 |
| 4 | valkey | 20,201 | 191 | 263 | 319 | 463 |
| 16 | dicedb | 58,944 | 271 | 375 | 455 | 1,647 |
| 16 | redis | 57,843 | 279 | 367 | 511 | 1,559 |
| 16 | valkey | 57,193 | 279 | 359 | 423 | 1,663 |
| 16 | zerodb | 36,340 | 403 | 792 | 996 | 12,488 |
| 16 | sqlite | 25,585 | 31 | 124 | 641 | 2,731,225 |

## Why

- **One device round trip, not two.** An append that grows a file makes `fdatasync` also commit the size change to the ext4 journal. Redis-family AOF grows on every write, so each ack costs about two round trips: 2.1 ms on Premium SSD v2. zerodb writes into zero-filled, preallocated segments, and SQLite mostly rewrites its WAL in place. Both pay one round trip, 0.7 ms.
- **Group commit.** zerodb syncs once for all queued writers. On the slow disk that turns 16 writers into 12× its 1-writer throughput. The Redis family groups too, but every group still pays the two round trips.
- **SQLite serializes writers.** Its writers take one lock. A loser sleeps in the busy handler for up to 100 ms per retry. So p50 stays low while throughput stops growing and the max reaches seconds: 2.7 s on NVMe and 49 s on Premium SSD v2 at 16 writers. The percentiles hide it, because the stalled inserts are under 0.1% of samples.
- **SQLite has no socket.** It runs in the writer's process. On fast NVMe with one writer that wins: 31 µs against zerodb's 89 µs, which includes an HTTP round trip over the Unix socket.
- **zerodb trailed at 16 writers on NVMe because of coordination, not parsing.** When the device answers in about 60 µs, a 200 µs wait to form a commit group cost more than the sync. With the wait off, the same VM gave 50.8k acks/s and p99 583 µs instead of 36.0k and 797. #28 removed the wait (D80). A perf profile of `zerodb serve` at 16 writers without it put 60% of samples on connection threads, 34% on the writer thread, about 18% in kernel wakeups and context switches, and about 5% in HTTP, JSON and the request log. Each append goes from a connection thread to the writer and back. Redis does it all in one event loop. Handoffs are the likely rest of the gap. #34 tests that, and #33 reruns this table on main.

## Not covered

PostgreSQL, Ultra Disk, replicated setups, reads, payloads other than 256 bytes, and writes without `fsync`.
