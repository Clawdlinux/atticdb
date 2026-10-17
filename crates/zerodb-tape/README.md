# Tape primitives

This crate provides CRC32C, file I/O, clocks, deterministic disk faults, and frame codecs.
It also provides a group-commit writer and footer-atomic crash recovery.

## Public API

Import `Tape`, `TapeOptions`, `Durability`, `RecordInput`, and `Committed` from the crate root.
Append failures return `WriteError` with an `Outcome`.
The root also exports `Error`, `Result`, `WriteResult`, and `RecoveryReport`.
Record types are `Frame`, `Header`, `Kind`, and `Digest`.
Storage backends use root exports `Io`, `StdIo`, `FileId`, and `LockGuard`.
Clocks use `Clock` and `SystemClock`. `SimDisk` remains public for fault injection.

Implementation modules, including directory, recovery, segment, and writer modules, are crate-private.
The `sim` module exposes deterministic driver helpers only with `cfg(test)` or the `sim` feature.
Use `sim::{WriterCore, PreparedBatch, WriteRequest, DataDir, SimClock}` for low-level tests.
Production `Tape` still compiles the writer core internally. The gate restricts external access, not production behavior.
Development self-dependency enables `sim` for integration tests and benchmarks, including plain `cargo test --workspace`.
Production builds don't enable that development feature. Missing public documentation is a compiler error.

## I/O and simulation

`Io` is object-safe and works through `Arc<dyn Io>`.
`StdIo` uses real files. Its parent directories must already exist.
`SimDisk` uses logical paths with implicit parent directories.
Both return sorted full paths from `list`.
Simulated directories exist when any file is below them. Directory listings include immediate subdirectories.
Listing a file or a missing directory returns an error.
Handles remain valid across rename and removal. Exclusive locks follow file identity.
Call `close` to release a handle. Closed handles return errors.
Open handles retain removed files until closed.
Simulated files remain available while durable directory entries or pending directory operations still reference them.

`SimClock` advances manually and can be shared through `Arc`.
`SystemClock` returns wall-clock nanoseconds and can move backward after clock adjustments.

`SimDisk::crash` returns an independent disk without changing the original.
The same seed and operations produce the same image.
Synced file bytes and synced directory entries survive.
Pending writes can disappear or reorder. Each sector can disappear, persist fully, or retain only a prefix.
Sectors persist independently, so later sectors can survive when earlier sectors are missing.
Sector boundaries default to 512 bytes. `set_sector_size` accepts positive sizes for smaller tests.
Pending directory operations can disappear. Each rename is atomic.
Cross-directory rename syncs persist both connected directories.
This is stronger than real filesystems that require both directories to be synced separately.

`fail_next_sync` fails the next file or directory sync.
Failed file syncs settle pending writes once through the seeded tearing model.
Cached bytes remain readable. Later crashes retain only their settled durable state.
Retrying sync returns success without healing doomed writes.
Later writes can sync independently without persisting earlier cached bytes.
`enospc_after_bytes` sets a remaining write budget. Exhaustion returns `ErrorKind::StorageFull`, possibly after writing a prefix.
`flip_bit` damages a durable byte. New crash images reset fault knobs and release locks.

## Frames

Frames use the 104-byte header and little-endian fields from the architecture document.
`encode` derives the CRC and total length.
It returns `InvalidFrame` for invalid structure or `BatchTooLarge` for total sizes above 4 MiB.
Normal record payloads start with a 16-byte salt.
Footer payloads contain exactly 44 bytes. Husks have no payload.

`decode` returns the first frame and its consumed byte count.
An empty slice returns `EndOfData`, including a segment filled to its final byte.
Trailing bytes belong to subsequent frames or preallocated space.
An incomplete fixed header returns `Truncated`.
A declared length beyond the supplied bytes returns `BadLength`.
This keeps single-bit length corruption classified as corruption, rather than an incomplete header.
End markers require both zero length and zero CRC.
Without that check, flipping a power-of-2 frame length could hide a damaged frame.
Unknown kinds, flags, and invalid structures return `BadLength`.
Unknown versions return `UnknownVersion` only after CRC validation.

`canon` contains exactly 30 bytes. Storage flags and payload length never enter the tenant chain.
Content digests retain the salt commitment after a payload becomes a husk.
Batch digests hash ordered content digests, not raw payloads.
`Tape::append` generates salts from `/dev/urandom` on caller threads.
Simulations use `PreparedBatch::with_salts` for deterministic salts.

## Writer and recovery

`Tape` drives `WriterCore` on a dedicated thread. Its last handle joins the thread when dropped.
Durable acknowledgements follow segment sync. Fast mode accepts only Cache records and returns no LSN.
`Tape::read` exposes complete, durable transactions.
Recovery verifies frame CRCs, transaction footers, content digests, and per-tenant chains.
Damage at or below the superblock returns `CorruptionDetected`. Invalid tails above it are truncated and synced.
Open removes segment files beyond the manifest before scanning. Directory sync makes cleanup durable, including retries.
Before publishing recovery, the tape rewrites retained bytes after the superblock boundary at the same offsets.
It then syncs each touched file and the directory.
A process restart can leave failed-writeback bytes readable in clean page cache. Sync alone may not persist them.
Rewriting re-dirties those bytes before new appends can depend on them.
The rewrite covers data written since the last superblock install, plus anything unsynced at a crash.
Under continuous load, that's at most about 1 second of writes. Delayed maintenance can extend that window.
Retained zero padding is also rewritten, including the final segment's unused capacity.
Otherwise failed tail-repair writeback could expose old garbage after a later roll and power loss.
Recovery syncs touched segments and the directory, then installs the recovered durable LSN before open returns.
Segments already protected by the superblock aren't rewritten or synced unless tail repair changes them.
Clean shutdown drains requests, syncs pending Fast writes, and installs the superblock without the 1-second limit.
Dropping `Tape` joins that shutdown before releasing the directory lock.
This protects short sessions from repeated historical rewrites and treating old durable damage as a torn tail.
Poisoned writers and failed final syncs don't advance the boundary.
Drop can't report shutdown errors. The next successful open recovers and checkpoints before accepting writes.

## Validation

Run from the workspace root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --release
cargo tree
```

`tests/sim.rs` runs 500 seeds during normal tests. Each seed covers 2 or 3 crash generations.
The driver uses `WriterCore::enqueue` and `step` without a writer thread.
It mixes 1 to 8 tenants, 1 to 64 records per batch, and Durable or Cache-only Fast writes.
Records span 0 to 8 KiB, with occasional 1 MiB requests.
The 64 KiB segment limit rejects oversized batches without poisoning the writer.
Supplemental scenarios use 2 MiB segments to verify accepted 1 MiB records and rolls.
Faults include sync failures, partial ENOSPC writes, queued crashes, and cuts between mutating I/O operations.
Some generations inject a sync failure, drop the poisoned writer, and reopen the live disk without power loss.
They verify survivors, continue appending, and then crash. Both stale and advanced superblocks are exercised.
Replies after a captured crash point remain uncertain.
The oracle checks complete transactions, exact content, LSN order, and tenant chains through `Tape::read`.
Recovered survivors become durable oracle entries across later generations.
Every 11th seed flips a random bit below the superblock and requires `CorruptionDetected`.
Offsets span all prefix segments through the boundary transaction's footer, including payloads and roll padding.
Some generations also cut recovery during open, covering tail repair, suffix rewriting, and manifest cleanup.
Failures print the seed and a replay command.

```sh
ZERODB_SIM_SEED=42 cargo test --features sim --test sim -- --nocapture
ZERODB_SIM_SEEDS=10000 cargo test --release --features sim --test sim
```

`ZERODB_SIM_SEED` selects the first seed. Alone, it runs 1 seed.
`ZERODB_SIM_SEEDS` selects the count. Without overrides, seeds start at 0.
The `sim` feature exports the test helper without affecting the production driver.
The 16-thread filesystem stress test verifies 20,000 Durable acknowledgements after reopen.
Debug builds ignore stress. The release command above runs it in CI.
Its scratch directory lives under `target/` and is removed after success.

```sh
cargo bench -p zerodb-tape --bench append -- --noplot
```

The benchmark compares bare framed writes against Durable append with 1 and 16 concurrent writers.
Record sizes are 256 B and 4 KiB. Each path preallocates 64 MiB segments.
Manual timing prints per-operation p50 and p99. Criterion records throughput and mean timings separately.
Hardware, commands, and measured results live in [bench/RESULTS.md](../../bench/RESULTS.md).
The macOS run isn't G1. G1 needs Linux NVMe measurements.

`proptest` runs without fork or timeout features. Tests need no external directory.
