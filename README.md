# zerodb

Agent memory database written in Rust. Fast and crash safe first.

Status: early. Milestone 1 (the tape) is done. Nothing here is ready for production use.

## Why

Agents need memory that does not lose writes and does not slow down as history grows. Today teams glue Redis, a vector DB and a memory framework together. We keep seeing the same failures:

- writes that report success but never persist;
- "last N turns" that gets slower as a session grows;
- scoped search that leaks or misses records;
- two processes sharing one memory file and overwriting each other.

zerodb puts session history, hybrid recall and a cache behind one durable commit point.

## Design in one paragraph

One append-only, checksummed, per-tenant hash-chained log (the tape) is the write-ahead log, the data store and the provenance chain at once. Every other structure is a view rebuilt from the tape. Group commit keeps the durable ack close to a bare append plus sync. Full design: [docs/design/0001-architecture.md](docs/design/0001-architecture.md). Decision log: [docs/design/0002-decision-log.md](docs/design/0002-decision-log.md).

## What exists today (M1)

- `crates/zerodb-tape`: frames, CRC32C, BLAKE3 tenant chains, segment files, manifest and superblock, a group-commit writer, and recovery.
- `SimDisk`: a deterministic fault-injecting disk. It loses, reorders and tears unsynced writes, fails syncs fsyncgate-style, flips bits, and runs out of space.
- A seeded simulation that crashes the writer at random points across 2 to 3 generations per seed and checks that no acked write is lost. 10,000 seeds pass.
- Benchmarks against a bare append-plus-sync baseline: [bench/RESULTS.md](bench/RESULTS.md).

## Roadmap

| Milestone | Scope |
|---|---|
| M1 | Tape, I/O, recovery, simulation. Done. |
| M2 | Session index, append and window, idempotency, checkpoints. Kill gate G1 on Linux NVMe. |
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
