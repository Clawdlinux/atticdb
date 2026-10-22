# zerodb-core

Durable session history on `zerodb-tape`. No async runtime or new third-party packages.

```rust,no_run
use zerodb_core::{Engine, EngineOptions, NewTurn, Role, Scope, WindowQuery};

let engine = Engine::open("memory", EngineOptions::default())?;
let scope = Scope {
    tenant: "acme".into(),
    agent: "helper".into(),
    session: "chat".into(),
};
let receipt = engine.append(
    &scope,
    vec![NewTurn {
        role: Role::User,
        content: b"hello".to_vec(),
        meta: Vec::new(),
    }],
    Some("request-1"),
)?;
let recent = engine.window(&scope, WindowQuery::Last(8))?;
# Ok::<(), zerodb_core::EngineError>(())
```

## Semantics

Sequences start at 1. `Since { seq, limit }` includes `seq`. Sequence 0 starts at 1.
`Last(0)` returns nothing. An unknown scope also returns nothing.
Windows pin a durable LSN and read each returned turn once.
The `window_reads` metric counts those `Tape::read` calls.
`window_budget(scope, query, max_bytes)` caps the sum of `content` and `meta` bytes.
`Last` keeps the newest turns that fit. `Since` keeps the oldest. Turns come back in sequence order.
Turns are never cut. A first turn over the budget returns no turns.
`Window::truncated` is true when the budget, not the query, ended the window.
A cut window costs 1 extra read for the turn it left out.

Append receipts cover turn LSNs, excluding any preceding tenant record.
The writer assigns sequences during preparation. Publication installs slots before replying.
Sequence receipts travel with the durable acknowledgement. Returning them never reads the tape.
A failed sync hides reservations and poisons writes. Reads keep the last durable view.
Every error carries `Outcome`. Receipt reconstruction failures after sync report `Committed`.
Preparation preserves `InvalidInput`, `CorruptIndex`, and `Poisoned` error kinds through the tape hook.

Retry keys use `(tenant, session, key)`, without agent.
The digest covers the ordered encoded turns, with sequences normalized to 0.
Each payload includes its byte length in that digest.
Keyless appends skip the retry digest. Keyed appends compute it on the caller.
Preparation and publication parse borrowed payload views without copying turn content.
Location-bound frame CRCs cover every payload, including assigned sequences.
Changing an agent, role, metadata, content, or batch boundary produces a conflict.
Retries return the original transaction, turn LSNs, and sequences.
They set `Appended::deduplicated`. Fresh commits leave it false.
Cached writer replies carry 1 marker byte after the 8-byte sequence. Dedup entries and checkpoints keep 8 bytes.
Keys outside the count window are new requests.
`dedup_window_effective` reports the smallest retained count among tenants with retained keys.

Tenant creation is atomic with its first turns. `TenantCreate = 6` consumes 1 record LSN.
The first tenant transaction allows 1023 turns. Later transactions allow 1024.
The tape's 4 MiB encoded batch limit includes headers and the footer.
That limit can reject content below the codec's independent 4 MiB field limit.

## Facts

```rust,no_run
use zerodb_core::{Engine, EngineOptions, FactQuery, NewFact};

let engine = Engine::open("memory", EngineOptions::default())?;
let rule = NewFact {
    key: "repo/payments/retry-rule".into(),
    content: b"Retry 3 times.".to_vec(),
    meta: Vec::new(),
    agent: "helper".into(),
};
let put = engine.fact_put("acme", rule, Some("request-1"), Some(0))?;
let current = engine.fact_get("acme", "repo/payments/retry-rule")?;
let page = engine.fact_list(
    "acme",
    FactQuery { prefix: "repo/".into(), after: None, limit: 50, max_bytes: 1 << 20 },
)?;
engine.fact_delete("acme", "repo/payments/retry-rule", "helper", None, Some(put.lsn))?;
# Ok::<(), zerodb_core::EngineError>(())
```

A write carries one fact, after an optional tenant record. Facts never share a batch with turns.
`source_refs` must be empty. The public API never sets them.
A key's current value is its newest revision by LSN. A tombstone means it has none.
Reads never fall back to an older revision.
`FactWritten` holds `txn`, `lsn` and `deduplicated`. Its `lsn` skips a leading tenant record.

`if_lsn` makes a write conditional on the key's current LSN. `0` means no current value.
The writer checks it at prepare, after the retry lookup, against writes staged in its group and then the published index.
A mismatch returns `ErrorKind::Conflict` with `Outcome::NotCommitted`. Nothing is written.
A write staged earlier in the same group can cause the conflict and then fail its sync.
An immediate re-read can then show the LSN that conflicted. Read again and retry.
`if_lsn` is not part of the retry identity. A retry with the same key and payload returns the original receipt.
Retry keys use `(tenant, fact key, idem_key)`. A session and a fact key with one name never collide.

Readers take the state lock, then the durable LSN, as windows do.
They use the newest revision at or below that LSN, so a group that is not acknowledged yet stays hidden.
`fact_history` returns at most `fact_history` revisions, newest first, tombstones included.
`truncated` is true when older revisions exist, or when `max_bytes` stopped the call.
`fact_list` walks keys under a byte prefix in key order. It visits at most 1,024 keys per call, tombstones included.
`next` is the last key the walk finished with. A page can hold no facts and still have a `next`.
A byte budget always returns the first fact or revision. Later ones stop before `max_bytes` is passed.
The budget counts key, agent, content and meta bytes, so a JSON page stays under 6 times `max_bytes` plus fixed fields.
`fact_reads` counts every tape read of a fact read, including the one a budget cut. `facts` counts keys.
`fact_revisions` counts retained pointers.

`max_facts` caps keys across tenants (default 100,000, at least 1). A new key at the cap returns `ResourceExhausted("facts")`.
Updates and deletes of existing keys always pass. Tombstones keep their slots, and deleting an unknown key takes one.
A lowered `max_facts` makes open fail with the same error until the cap is raised, as `max_sessions` does.
`fact_history` defaults to 16, from 1 to 4,096. A key holds at most `fact_history + 2` pointers.
One reports `truncated`. One keeps the value readers at the previous durable LSN still see.
The tape keeps every revision.

Checkpoints write tag 4 fact entries and tag 5 fact retry entries.
Tag 4 holds the newest `fact_history + 1` revisions, the `fact_history` they were written under, and a bit that says older ones were dropped.
A larger `fact_history` at open replays the tape. A smaller one trims on load.
Install clears a key's dirty mark only if its newest LSN did not change after the snapshot.
When at least half the keys are dirty, the snapshot walks the whole map.
An S3 binary aborts replay on a `Fact` frame, so it cannot open a directory with facts.

## Health

`Engine::health()` returns `Health::Ok`, `Health::ReadOnly`, or `Health::Poisoned`.
Health reflects observed failures. It doesn't query disk space.
Health probes never wait for the shared-state mutex.
`Ok` means no write-blocking failure has been observed.
The tape checks free space before preparing each write group.
Its default floor is `min(1 GiB, 5% of total volume bytes)`, chosen at open.
Override it with `EngineOptions::tape.free_space_floor = Some(bytes)`.
The check budgets group bytes and new segment reservations before comparing the remaining space with the floor.
Below the floor, new appends return `ErrorKind::Storage(zerodb_tape::Error::DiskFull)` with `Outcome::NotCommitted`.
Clean full-disk failures during segment creation or preallocation return the same error and outcome.
Linux and macOS preallocate through rustix. Unsupported operations and other platforms fall back to sparse `set_len`.
Preallocation isn't a guarantee on APFS or btrfs. See [tape storage details](../zerodb-tape/README.md#io-and-simulation).
These rejections set health to `ReadOnly`. Reads keep serving acknowledged turns.
A failed space query also sets `ReadOnly`. Its original storage error and `NotCommitted` outcome pass through.
Only a later write that passes the space check clears `ReadOnly`.
Idle instances stay `ReadOnly`, even after space returns.
Callers and orchestrators must keep sending writes or retries to a `ReadOnly` instance. Don't drain it.
Space-check rejections don't add retry keys. Retry the same `idem_key` after space returns to commit normally.
Further retries return that durable receipt without writing again.
Committed keys return their original receipt even during a space outage.
Reusing a committed key with different payloads returns `IdempotencyConflict`, including during outages.
These retries check published dedup state before submission. They don't recheck space or clear `ReadOnly`.
The append path takes the shared-state mutex, as it already does for tenant lookup and session pinning.
`Poisoned` takes precedence over `ReadOnly` and requires reopening before writing.
It reports tape poisoning, shared-state failures, or a poisoned state lock.
Data-write, sync, and manifest failures poison the writer, even when disk exhaustion caused them.
Health describes writer availability. It doesn't guarantee every read will succeed after storage corruption.

## Payload format

Version 1 uses little-endian integers. Every length prefix is a u32 byte count.

```text
u8 version = 1
agent_id: length + UTF-8, <=255 bytes
session_id: length + UTF-8, <=255 bytes
u64 seq
u8 role: system=0, user=1, assistant=2, tool=3
meta: length + bytes, <=65536 bytes
content: length + bytes, <=4194304 bytes
u8 key_present: 0 or 1
if present: idem_key length + UTF-8, <=128 bytes
```

Unknown versions, invalid tags, malformed UTF-8, trailing bytes, and oversized fields are rejected.

## Bounds and cold pages

Page capacities are 16, 64, 256, 1024, then repeated 4096-slot pages.
Arithmetic maps sequences to page slots. Slots store LSNs, not segment offsets.

`EngineOptions` bounds resident slots, sessions, page metadata, tenants, and retry tables.
`Metrics::tape_index` reports the tape's separate resident budget, reloads, evictions and dirty writebacks.
G1 prints those counters after timing. The 1M-turn trace checks that eviction happened at the default 8 MiB budget.
Defaults allow 256 MiB of hot slots and 10,000 tenants.
Directory defaults allow 1M sessions and 8M pages, including cold pages.
Retry tables retain at most 1M keys per tenant and 8M globally.
Those limits apply to the published table. Eviction happens during publication, never preparation.
The writer keeps only its current group's unpublished entries in a separate overlay.
The overlay independently obeys the same per-tenant and global limits.
Temporary overshoot is at most that group's distinct staged keys, capped at 1M per tenant and 8M globally.
Thus published plus staged entries cannot exceed 2M per tenant or 16M globally.
Exceeding overlay capacity rejects a new keyed request with `ResourceExhausted("prepared_dedup")`.
Publication transfers entries into the published table and empties the overlay after a successful group.
Preparation doesn't clone or evict from the published table.
Checkpoint snapshots share immutable retry entries rather than copying their payloads.
Checkpoints capture published data while later writes are still prepared.
A failed sync leaves the published retry window unchanged.

Clean checkpoint-backed pages use LRU eviction. Active append tails stay pinned until acknowledgement.
Dirty pages need a worker checkpoint before eviction.
Memory pressure requests that checkpoint, then retries allocation once.
A batch needing more resident capacity than configured returns typed `ResourceExhausted`.
This also applies when pinned concurrent requests prevent eviction.

Cold windows read page records directly from checkpoint files, without scanning session history.
Recovery can spill completed transactions through a checkpoint worker when resident slots fill.

## Checkpoints

The checkpoint worker wakes every 60 seconds or after 64 MiB of published tape bytes.
An atomic pending flag coalesces byte-threshold notifications.
The worker clears it and rechecks the threshold under State before saving.
`checkpoint()` waits for an explicit worker snapshot.
Normal checkpoints write only dirty pages, newly registered tenants, and retained retry-key additions.
Deterministic oldest-first eviction rebuilds the retry window without tombstones.
Snapshots share immutable page buffers and retry entries through reference counts.
Appending to a shared page uses copy-on-write.
Cold reads run outside the state lock, then recheck the current backing location before installation.
Compaction can replace or unlink a captured location. Loading retries against the replacement.
File writes and syncs also run outside the state lock.
Checkpoint encoding uses a buffer capped at 4 MiB.
Full buffers write at increasing file offsets. Records never span buffer flushes.
The file sync runs after all chunks, not after each chunk.
`checkpoint_buffer_peak_bytes` reports peak occupied encoding bytes among completed checkpoints.
Snapshot metadata and bounded per-record scratch are separate from that buffer.
Backing locations install in chunks of 32 pages, releasing State between chunks.
Old-file removal and directory sync happen after releasing State.

Full snapshots still enumerate page and retry-entry metadata under the state lock.
That work scales with configured directory bounds, not dirty bytes.
It can delay writer preparation and publication for large directories.
`checkpoint_snapshot_lock_ns` reports the longest snapshot hold, excluding lock acquisition.
`checkpoint_state_lock_ns` reports the maximum snapshot, install-chunk, or final watermark hold.
This includes installation work. It excludes waiting to acquire State.
`checkpoint_saves` counts successful installs during the current open.
The `full_snapshot_shares_pages_and_measures_its_lock_hold` test measures a 1000-page full snapshot.
This measures hold time without asserting a hardware-dependent latency threshold.

Attempt 0 uses `ckpt-{lsn}.zdb`. Later attempts use `ckpt-{lsn}-{attempt}.zdb`.
State reserves a monotonic attempt ID before filename checks or file creation.
Failures and existing-name collisions consume IDs. Replay fallback preserves the counter.
Retries cannot reuse a filename during that engine state's lifetime.
Failures before a rename attempt remove partial data and staging files.
An ambiguous rename keeps the complete file until the next open checks the installed chain.
Open removes unreferenced checkpoint files after acquiring the tape lock.
Every record contains a CRC32C, a total length, and a typed payload.
CRC coverage includes the length and payload.
File headers use `ZCK2` and carry the watermark and transaction.
The `ZCM2` manifest names the ordered file chain and its final watermark.
Each entry includes the file's exact length and BLAKE3 digest over all encoded bytes.
Load checks length and hashes the existing CRC-framed read stream.
Either mismatch logs a warning and triggers full tape replay.
Legacy `ZCM1` manifests also trigger full replay.

Installation syncs the data file before replacing `CHECKPOINT`.
The replacement uses a synced staging file, rename, then one directory sync.
There is no directory sync between data and manifest installation.
After a crash, a durable manifest can reference a missing derived filename.
Open logs a warning and rebuilds from the authoritative tape in that case.
The 9th checkpoint compacts the chain into 1 file.
Old chain files are removed only after installation and backing-location replacement.

Open validates checkpoint CRCs, loads page locations, and replays application records above the watermark.
Corrupt or missing checkpoint data logs a warning and triggers full replay.
Checkpoints are derived data. They never authorize truncating tape records.
Retry checkpoint records include the original sequence receipt.
Every `ZCK1` file falls back to full tape replay, rather than guessing its receipt layout.
The tape payload format is unchanged.
Tape recovery still verifies the entire tape before application replay.
The replay counter excludes checkpointed records. It does not measure tape verification work.

`Engine::open_with` accepts the tape's `Io` and `Clock` traits for SimDisk tests.
Real filesystem callers must create the directory before using that method.
`Engine::open` creates it automatically.

## Deterministic engine simulation

The `sim` feature enables the engine fault harness:

```sh
cargo test -p zerodb-core --release --features sim --test engine_sim
ZERODB_SIM_SEEDS=10000 cargo test -p zerodb-core --release --features sim --test engine_sim
ZERODB_SIM_SEED=19 cargo test -p zerodb-core --release --features sim --test engine_sim
```

The default is 500 seeds. `ZERODB_SIM_SEED` selects 1 seed unless `ZERODB_SIM_SEEDS` also sets a count.
Failures print the seed and reproduction command.
Each seed runs 3 crash generations across 48 scopes and 4 tenants.
It also runs grouped generations with 2 through 4 distinct appends per writer step.
Those groups include same-group retries, conflicts, and multi-batch publication.
The oracle checks bytes, dense sequences, retry receipts, and window read counts.
It also checks transaction atomicity, tenant IDs, and corrupt-checkpoint fallback.
Sync failures cover errors before persistence and errors after persistence.
Live reopen preserves failed page-cache writes. Crash reopen uses a captured SimDisk image.
A captured mid-append cut does not excuse later acknowledged loss.
When append returns successfully, a second post-return crash image must retain its receipt.
Keyless turns carry unique oracle metadata, so their uncertain outcomes remain independently checkable.
Checkpoint corruption flips page-record checksum bytes, not file magic.
S1 also flips tape payload and length bytes above the superblock.
Later footers must prove corruption. Without a proof, recovery reports a truncated tail.
Some seeds constrain the tape index to 12 KiB, forcing page reloads during recovery.

`TapeOptions::inline_sim` disables background workers.
It is feature-gated and defaults to false. Never enable it for real storage.
Explicit seeded checkpoints replace the wall-clock timer.
`enqueue_sim` queues appends. `step_sim` runs their shared production writer step.
Register tenants before queueing grouped appends. `SimAppend::finish` receives post-publication receipts.
Seeded I/O callbacks invoke checkpoints between preparation and publication.
They also run queued writer steps during a checkpoint's own I/O, between snapshot and install.
The regression requires a later checkpoint to retain newly published page revisions after reopen.
Removing the revision guard fails at seed 0 and in the dedicated regression.
`checkpoint_step_sim` services pending byte-threshold wakes through the production wake check.
Memory-pressure checkpoints and recovery spills also run inline.
Production defaults retain worker threads.

## G1 performance harness

```sh
./bench/g1.sh
G1_TURNS=65536 ./bench/g1.sh
G1_BATCH=1 G1_TURNS=65536 ./bench/g1.sh
G1_TURNS=192000 G1_REPS=3 G1_HISTORY=0 ./bench/g1.sh
G1_TURNS=208000 G1_REPS=3 G1_REPLAY_PROBE=1 ./bench/g1.sh
```

The default trace appends 1M turns using 16 writers across 1024 sessions.
`G1_CHECKPOINT_MIB` overrides the checkpoint byte threshold. Its default remains 64.
The replay probe alternates 64 MiB and 128 MiB across repetitions.
It measures clean reopen wall time, application replay count, and sampled pre-close pending bytes.
That time includes full tape verification. It does not measure crash-repair rewriting.
`G1_REPS` defaults to 3 and cannot be smaller.
Each repetition runs bare, Tape, then Engine. Summaries report medians and min/max spreads.
Ratios are paired within repetitions before computing their median.
`G1_HISTORY=0` skips only the separate history experiment.
Each durable batch contains 4 turns. Content sizes cycle through 256, 512, 1024, 4096, and 8192 bytes.
`G1_BATCH` accepts 1 through 256. `G1_WRITERS` sets the closed-loop writers, 16 by default, 1 through 100, so agent ids keep one width. `G1_TURNS` must be divisible by `G1_WRITERS * G1_BATCH`.
The engine runs with 2 concurrent last-16 window readers and default checkpoint settings.
Every session starts with 1 untimed turn. Timed writes exclude tenant creation.
Durable-ack latency includes caller preparation and queueing. Quantiles use nearest rank per batch, not per turn.
Reader latency samples every 64th completed query. All queries contribute to the count.
Reopen verifies every session's final sequence outside timing.

The bare baseline reuses M1's framing, segment preallocation, append, and sync code.
It preframes the same turn codec, metadata, transaction footer, and byte counts.
Its fixed headers exclude hashing and caller preparation, as in M1.
All paths use the same content-size trace and transaction batching.
Bare and Tape share the same adaptive coalescer.
`TapeOptions::coalescing_window` defaults to 200 us. Zero disables waiting. Larger values are rejected.
It targets the largest of the previous 64 groups until that bounded timeout.
After waiting, it drains ready arrivals without blocking again.
Group boundaries still depend on scheduling. Every path reports its observed request-group size.
Tape computes frame CRCs after reserving each transaction's segment and offset.
Periodic superblocks run on a helper using only published, already-synced LSNs.
Requests follow segment rolls, 30 seconds of active writes, or 256 MiB of published bytes.
`TapeOptions::superblock_interval` and `superblock_bytes` configure the latter thresholds. Both must be positive.
The byte count includes roll padding. Requests can overshoot by one group.
Deferred installation increases recovery rewrites above the last completed superblock.
Repair also includes final unused preallocation. Helper scheduling and I/O can extend that window.
Acknowledgements never wait for the metadata helper.
Shutdown joins that helper before the final synchronous install. Open retains its synchronous install.
Metadata failures poison later writes. They never invalidate earlier durable acknowledgements.
Tape also runs separately to distinguish tape overhead from engine and reader overhead.
Only the engine runs window readers.
Segment rolls remain inside timing. Initial segment setup and final shutdown stay outside.

The history test holds returned size at 16 for histories of 16, 1024, and 65536 turns.
It reports last/since latency after warmup and asserts exactly 16 tape reads per query.
This proves flat record-read work. Timing still depends on filesystem caching and scheduling.
Measurements live in `bench/RESULTS.md`.
macOS uses `sync_data`, which invokes `F_FULLFSYNC` in this Rust toolchain.
The local macOS target is median Engine/bare durable-ack p99 at most 1.5x.
This is an append-only proxy. Facts and forget are not measured.
G1 remains pending until Linux NVMe runs the full trace and nightly LazyFS crash checks.
