# Tape primitives

An append-only log with group commit, location-bound CRC32C frames and whole-transaction recovery.
The current format and scope are defined in [0003](../../docs/design/0003-slim-v0.1.md).

## Public API

Import storage types from `zerodb_tape`.
`Tape::open(io, clock, directory, options)` locks the directory and starts the writer.
`Tape::open_with(state_machine, io, clock, directory, options)` also attaches application state.
Real directories must already exist.
`TapeOptions::server_address` is written into `LOCK`. `zerodb serve` puts its instance id and socket path there.

`Tape::append(records, Durability::Durable)` returns `Committed::Durable` or `WriteError`.
The receipt contains the transaction ID, inclusive record LSN range and application reply.
Errors carry an `Outcome`: `NotCommitted`, `Unknown` or `Committed`.
There is no lossy acknowledgement mode.

`Tape::read(lsn)` returns a published record, or `None` outside the durable range.
`Tape::scan(from_lsn)` captures a durable boundary and yields application records in LSN order.
Footers are not application records.

`Frame`, `Header` and `Kind` describe records.
`Digest` is an application digest type, not a tape-integrity mechanism.
`Io`, `StdIo`, `FileId` and `LockGuard` provide storage access.
`Clock` and `SystemClock` provide timestamps.

Implementation modules are private. The `sim` feature exposes deterministic test helpers.
Use `sim::{WriterCore, PreparedBatch, WriteRequest, DataDir, SimClock}` in storage tests.
Development dependencies enable those helpers for integration tests and benchmarks.
Production builds do not enable them by default.

## Frame v2

Fields are little-endian. The fixed header occupies 40 bytes.

```text
u32 crc32c
u32 len
u8  kind
u8  version = 2
u16 flags = 0
u64 txn
u64 lsn
u64 commit_time_ns
u32 tenant_id
payload
```

The CRC covers `segment_id_le || offset_le || bytes[4..len)`.
Both location values are u64 integers.
The segment ID comes from the manifest, not a parsed filename.
Copying a valid frame to another location invalidates its CRC.
Frames at a reused location also need a matching transaction footer.
CRC32C detects damage, not malicious edits. Someone with directory write access can forge frames and footers.

Kinds are Turn 1, Fact 2, Forget 3, Footer 5 and TenantCreate 6.
Kind 4 is reserved.
Every record consumes 1 dense LSN. A footer consumes none and repeats its final record's LSN.
Transaction IDs are dense.
TenantCreate belongs to a normal transaction and carries the new tenant's ID.

The footer payload is exactly 32 bytes:

```text
u64 txn
u64 first_lsn
u64 synced_lsn
u32 record_count
u32 records_crc
```

`records_crc` covers the concatenated 4-byte stored record CRCs.
The footer must match exactly the records it closes.
`synced_lsn` is the writer's durable LSN when it builds the footer.
It never advances merely because another transaction was written.
Footers in 1 unsynced group carry the previous group's durable LSN.

Frames and encoded transactions are limited to 4 MiB. A transaction contains at most 1024 records.
Zero flags, known kinds and supported versions are required.
Frame v1 is rejected with a clear error. There is no migration.
An end marker requires both zero CRC and zero length.
Unknown versions, malformed lengths and invalid structures are rejected.

## Writer and publication

Caller-side preparation checks sizes. The writer assigns LSNs and reserves the final disk location before calculating CRCs.
`StateMachine::prepare` may rewrite a batch, reject it, or return a cached reply.
`prepare` must not grow a batch past its submitted encoded size. Shrinking and equal-size rewrites are allowed.
Rewrites are validated before framing against the size budgeted by the free-space check and segment reservations.
Growth fails before writing and poisons the writer, even when it fits a reserved segment.
Preparation may already have reserved application state, so reopening is required after an invalid rewrite.
Preparation reservations must stay private until publication.

The writer syncs every dirty segment in the group before calling `publish`.
It advances the durable LSN only after publication, then releases replies.
Cached replies share that sync and publication outcome.
A failed write or sync poisons the writer. Later appends are rejected.
Reads retain the published view.

At open, the free-space floor defaults to `min(1 GiB, 5% of total volume bytes)`.
Set `TapeOptions::free_space_floor` to `Some(bytes)` to override it.
The writer checks space once per group, before application preparation.
It budgets encoded bytes, new segments, and LSN-index backing growth before comparing the remainder with the floor.
A floor hit rejects the whole group with `DiskFull` and `NotCommitted`, without preparing application state.
Required segments and LSN-index backing pages are reserved with `Io::allocate` before application preparation.
The highest possible group LSN bounds growth of `LSN-OFFSETS` and `LSN-ROUTES`, rounded up to 1 MiB chunks per file.
A fresh open needs about 2 MiB above the floor to start read-write, which only matters on tiny volumes or very small explicit floors.
Open attempts index reservations after recovery, only when the space query and floor allow them.
Preallocated index backing lets cold reads evict dirty pages without new blocks or an increased cache budget.
Without it, a full disk keeps dirty pages resident. Cold reads evict a clean page or exceed the budget by one page.
Recovery recreates these derived files and rebuilds resident pages before attempting optional backing reservations.
An index fitting its cache can reopen without allocating backing blocks.
Full-disk or space-query failures defer reservations and start read-only, without poisoning.
The next write retries outstanding reservations and any unknown default-floor query before `prepare`.
Sparse fallback relies on the floor; APFS and btrfs retain the same allocation caveat as segments.
Clean full-disk failures during segment creation or allocation return the same error and outcome.
These rejections set `read_only()` without poisoning. A later write's passing space check clears it.
Idle time and health probes never clear read-only, even after space returns.
Failed allocations remove their segment files.
Unused prepared segments can serve later groups before shutdown.
A roll takes the zero-filled `spare.zdb` when one is ready, and preallocates otherwise.
On ext4, preallocated blocks are unwritten extents, so each sync after a first write also commits the journal.
Zeroed blocks make each segment sync a data flush only.
Open fills the first spare before the writer starts. The maintenance thread refills it after each roll.
The fill preallocates, then writes zeros in 256 KiB chunks with a sync after each, so tape syncs never queue behind a large flush.
It skips the fill unless two segments of room stay above the floor. The space check counts a ready spare as one prepared segment.
Open deletes leftover `spare.tmp` and `spare.zdb` files.
Copy-on-write filesystems such as btrfs, ZFS and APFS gain nothing from zero-fill and pay one extra segment write.
Recovery accepts fully zero, full-sized trailing reservations as unused, regardless of their size or count.
It drops surplus reservations without a torn-tail warning and may keep one empty segment as the append target.
Nonzero fragments and gaps before later data retain corruption checks.
A failed space query also sets `read_only()` and rejects the group without poisoning.
The original storage error and `NotCommitted` outcome pass through unchanged.
Segment sync, directory sync, manifest failures, and ambiguous cleanup failures still poison the writer.
Unlike read-only, poisoning requires reopening before writes can resume.
The core's non-blocking `Engine::health()` reports `Ok`, `ReadOnly`, or `Poisoned`. Poisoning takes precedence.
See [core health](../zerodb-core/README.md#health) for retry behavior.

Commit timestamps use `max(previous + 1 ns, clock)`.
Open recovers the previous timestamp from the tape.
A backwards wall clock cannot move later commits backwards.

During active writes, superblock maintenance is requested after 30 seconds, 256 MiB, or a published segment roll.
Options control the time and byte limits.
Clean shutdown drains requests, syncs data and installs the final superblock.
Dropping the last Tape joins the writer before releasing its directory lock.
Drop cannot report shutdown errors. Reopening performs recovery before accepting writes.

## Recovery

Recovery checks location-seeded frame CRCs, dense LSNs and transactions, footer fields and record CRC binding.
Footer guards require `previous_synced_lsn <= synced_lsn < first_lsn`.
Damage below the superblock returns `CorruptionDetected`.

Above that floor, recovery scans ahead before discarding a bad tail.
Let `d` be the last retained transaction's LSN plus 1.
Only whole valid transactions with `first_lsn >= d` count as candidates.
A candidate footer with `synced_lsn >= d` proves that dropped data had already synced.
That proof returns `CorruptionDetected`, even if the damaged footer contains garbage.
The reported bad LSN is diagnostic only. Damaged bytes never choose the proof boundary.

Lookahead scans bytes across every later manifest segment and skips zero runs.
Decoded records are not re-encoded or repeatedly scanned as transaction suffixes.
A footer can select a valid overlapping suffix after a false start.
Without a proving footer, recovery truncates, zeros and syncs the tail.
The report and log identify the dropped LSN range and byte count.
Damage inside the last synced group can lack a later proof. This is I1's stated limit.

Before returning recovered data, recovery rewrites retained bytes above the old superblock and syncs them.
A failed writeback can leave readable clean-cache bytes that another sync cannot repair.
Rewriting re-dirties those bytes. Retained roll padding is also rewritten.
Only after recovery syncs can open advance the durable floor.
Checkpoints reduce application replay, not the full tape CRC scan.

### Explicit operator cut

Close the database before calling `Tape::recover_truncate_at(directory, lsn)`.
`recover_truncate_at_with(io, directory, lsn)` supports a replaceable I/O backend.
The cut must be 0 or the final LSN of a whole transaction.
Interior cuts fail with both nearest valid boundaries. Cuts beyond the log name its last valid boundary.
The entire retained prefix must validate. Otherwise the error identifies the first bad LSN.
Malformed discarded segments do not prevent a lower valid cut.

A CRC-protected intent is persisted before changing data.
Data is truncated, zeroed and synced before the superblock is lowered.
The operator cut never raises that floor. Normal recovery handles retained data above it.
Open resumes an interrupted intent before checking the floor.
Reports and logs include the cut, dropped range and dropped bytes.
Dropped LSNs may be reused, including acknowledged LSNs. D73 documents this explicit exception to I3.
`zerodb recover --truncate-at <lsn> --data-dir <dir>` runs the cut from the command line.

## Paged LSN index

`LsnIndex` stores per-segment arrays of offsets, indexed by `lsn - segment_first_lsn`.
Its routing directory is paged too. Lookups need a fixed number of page accesses.
`TapeOptions::lsn_index_bytes` bounds resident pages and bookkeeping.
The default is 8 MiB. The minimum is 4224 bytes.
Each 4096-byte page is charged another 128 bytes for cache bookkeeping.
Eviction uses FIFO. Dirty evictions write derived pages, without adding a tape sync.

`LSN-OFFSETS` stores 8 bytes per record. `LSN-ROUTES` stores 16 routing bytes per record.
Files are padded to 1 MiB chunks and rebuilt at every open.
They are derived indexes, not recovery authorities.
Reads still verify the requested LSN and location-bound frame CRC.
`Tape::index_stats()` reports resident bytes, the budget, evictions, dirty evictions and page reloads.

## I/O and simulation

`Io` is object-safe and shared through `Arc<dyn Io>`.
Handles survive rename and removal until closed. Exclusive locks follow file identity.
Both backends return sorted full paths from directory listings.
`StdIo` uses real files. `SimDisk` uses logical paths.
`SimDisk` requires the `sim` feature outside unit tests. Production builds omit it.
`SimClock` advances manually. `SystemClock` can move backwards.

`StdIo::allocate` reserves blocks through rustix on Linux and macOS for empty or shorter files.
It then exposes the requested file length.
On macOS, rustix tries contiguous `F_PREALLOCATE`, then `F_ALLOCATEALL`, and calls `ftruncate`.
Equal or smaller macOS lengths skip preallocation, which would reserve beyond the current physical end.
Zero lengths skip preallocation. Other platforms fall back to sparse `set_len`.
`EOPNOTSUPP`, `ENOTSUP`, and Linux `ENOSYS` also fall back to sparse `set_len`.
`ENOSPC` remains `StorageFull`. Other errors propagate.
Preallocation does not guarantee later writes on APFS or btrfs. The free-space floor remains the guard.
`SimDisk` charges reserved or written 512-byte blocks, including metadata.
Its `set_len` extension stays sparse until written or allocated. Truncation frees the discarded blocks.
Partial-write cleanup truncates and tries to re-reserve the tail with `allocate`.
Any reservation error falls back to sparse `set_len`, so the successful prefix can still sync and publish.
The failed write then reports `NotCommitted`. The writer remains poisoned.
Truncation, resize, or sync failures can still report `Unknown`.

`SimDisk::crash()` returns an independent image without changing the original.
The same seed and operations produce the same image.
Synced bytes and directory entries survive. Unsynced sectors can disappear, reorder or retain only a prefix.
The default sector size is 512 bytes.
Renames are atomic. Unsynced directory operations can disappear.

`fail_next_sync()` settles pending writes once through the tearing model, then reports failure.
`storage_full_next_sync()` uses the same model with a typed `StorageFull` error.
Clean cache bytes can remain readable without later writeback. Retrying sync does not heal them.
New writes can still persist independently.
`enospc_after_bytes()` allows partial writes before reporting a full disk.
`storage_full_returns()` counts actual write, allocation, and sync `StorageFull` errors per crash generation.
Bit-flip helpers damage durable bytes.
Crash images reset fault controls and release locks.

The seeded harness keeps transaction atomicity and acknowledged-write survival checks.
It also tests garbage footers, misplaced frames, torn groups, operator-cut crashes and bounded-index reloads.
Reply-only syncs and reordered writes must not create false corruption proofs.

## Validation

Run from the workspace root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --release
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-tape --release --features sim --test sim
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-core --release --features sim --test engine_sim
```

The default tape run uses 500 seeds. CI sets 2,000 explicitly.
`ZERODB_SIM_SEED` selects a starting seed. Failures print a reproduction command.
Release tests also run the real-filesystem stress test.
