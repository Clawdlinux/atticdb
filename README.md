# zerodb

Agent memory database written in Rust. Durable writes and fast recent-history reads.

Status: early. The tape, session engine and S1 storage changes are implemented. This isn't ready for production.

## Why

We want memory that keeps acknowledged writes and stays fast as history grows.
Session history lives in 1 append-only log. Every index can be rebuilt from that log.
Facts, forget and export come in later steps. Search is deferred.

The current scope is [0003: slim v0.1](docs/design/0003-slim-v0.1.md).
The [decision log](docs/design/0002-decision-log.md) explains the choices.
[0001](docs/design/0001-architecture.md) retains historical mechanics, not the current feature scope.

## What exists

- `zerodb-tape`: location-bound CRC32C frames, whole-transaction footers, group commit and guarded recovery.
- `zerodb-core`: `append`, `window`, tenant isolation, idempotent retries and incremental checkpoints.
- `SimDisk`: seeded crashes, reordered writes, torn sectors, failed syncs, bit flips and full-disk faults.

Frame v2 uses a 40-byte header instead of 104 bytes. Its footer payload is 32 bytes.
Record LSNs and transaction IDs are dense. Footers repeat the final record LSN.
Every write waits for sync and publication. There is no lossy acknowledgement mode.
Commit timestamps stay monotonic across restarts, even when the clock moves backwards.

`Fact` and `Forget` payload v1 codecs exist. The engine rejects these kinds until their behavior ships.
There are no hash chains, signatures or tamper-evidence guarantees.
Someone with write access to the directory can edit history.

### Tape hooks

`Tape::open_with(state_machine, io, clock, path, options)` attaches application state.
`Tape::open(...)` uses `NoopStateMachine`.
The writer runs `prepare` once per batch, in queue order.
`PreparedBatch::records_mut()` allows validated rewrites before framing.
`Prepare::Reject` and `Prepare::Reply` write nothing.

Keep preparation reservations private until `publish`.
Cached replies share the original group's sync and publication outcome.
A failed sync poisons writes and returns `Unknown`. Reads retain the published view.
Publication runs before the durable LSN advances and before replies leave the writer.

Recovery verifies and syncs the tape before ordered `replay` callbacks begin.
Set `TapeOptions::replay_from_lsn` to the loaded checkpoint watermark.
`Tape::scan(from_lsn)` captures a durable boundary and yields application records, without footers.

### Bounds and recovery

`TapeOptions::lsn_index_bytes` bounds resident offset pages and their routing directory. The default is 8 MiB.
Per-segment offset arrays have arithmetic slots. Their routing directory is also paged.
Lookup needs a fixed number of page accesses, regardless of log length.
The derived `LSN-OFFSETS` and `LSN-ROUTES` files are rebuilt at open.

Open verifies the whole tape with CRC32C. Checkpoints reduce application replay, not that full scan.
Open time therefore grows with log size. There is no compaction yet.

Damage below the superblock is `CorruptionDetected`.
Above it, a later validated footer can prove that damaged records were already synced.
Without that proof, recovery truncates and reports the discarded range and byte count.
Damage inside the last synced group can lack a later proof. See I1 in 0003.

`Tape::recover_truncate_at(directory, lsn)` is an explicit operator escape. Close the database first.
Use 0 or the final LSN of a whole transaction.
An interior cut fails with both nearest valid boundaries. The retained prefix must validate.
A durable intent resumes an interrupted cut before normal recovery checks the superblock.
The report and log name the cut, dropped range and byte count.
Dropped LSNs may be reused, including acknowledged LSNs. D73 documents this exception to I3.
CLI wiring arrives with the server binary in S3.

### Session engine

The writer assigns dense session sequences. Windows cost O(returned), not O(history).
Retry keys return original receipts within bounded tenant and global windows.
Session pages use a resident-memory budget. Cold pages reload from checkpoints.
A bad checkpoint triggers full replay with a warning.

See [the core API notes](crates/zerodb-core/README.md) for formats and limits.
See [benchmark results](bench/RESULTS.md) for the measured macOS workload and its limits.

## Next steps

| Step | Scope |
|---|---|
| S1 | Frame v2, guarded recovery, bounded tape index. |
| S2 | Disk-full floor and real preallocation. |
| S3 | Shared server, auth, HTTP, MCP and Python client. |
| S4 | Tenant-scoped fact revisions and conditional updates. |
| S5 | Hide requests and markdown export. |
| S6 | Dogfood and measure the remaining gaps. |

## Build and test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --release
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-tape --release --features sim --test sim
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-core --release --features sim --test engine_sim
```

See [TESTING.md](TESTING.md) before adding or changing coverage.
Each crate has 1 integration binary in `tests/it/main.rs`.
Behavior modules share fixtures through `tests/it/support.rs`.
The `sim` and `engine_sim` targets keep their CI names.

Run a behavior module with `cargo test -p zerodb-tape --test it recovery::`.
Filesystem stress is opt-in. The separate `slow-tests` CI job runs it:

```sh
cargo test --workspace --release -- --ignored
```

## License

MIT OR Apache-2.0.
