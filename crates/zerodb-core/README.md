# zerodb-core

Durable session history on `zerodb-tape`. No async runtime or new third-party packages.

```rust,no_run
use zerodb_core::{Engine, EngineOptions, NewTurn, Role, Scope, Trust, WindowQuery};

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
        trust: Trust::Untrusted,
    }],
    Some("request-1"),
)?;
let recent = engine.window(&scope, WindowQuery::Last(8))?;
# Ok::<(), zerodb_core::EngineError>(())
```

## Semantics

Sequences start at 1. `Since { seq, limit }` includes `seq`. Sequence 0 starts at 1.
`Last(0)` returns nothing. An unknown scope also returns nothing.
Windows include every trust label. They pin a durable LSN and read each returned turn once.
The `window_reads` metric counts those `Tape::read` calls.

Append receipts cover turn LSNs, excluding any preceding tenant record.
The writer assigns sequences during preparation. Publication installs slots before replying.
A failed sync hides reservations and poisons writes. Reads keep the last durable view.
Every error carries `Outcome`. Receipt reconstruction failures after sync report `Committed`.
Preparation preserves `InvalidInput`, `CorruptIndex`, and `Poisoned` error kinds through the tape hook.

Retry keys use `(tenant, session, key)`, without agent.
The digest covers the ordered encoded turns, with sequences normalized to 0.
Each payload includes its byte length in that digest.
Changing an agent, role, trust, metadata, content, or batch boundary produces a conflict.
Retries return the original transaction, turn LSNs, and sequences.
Keys outside the count window are new requests.
`dedup_window_effective` reports the smallest retained count among tenants with retained keys.

Tenant creation is atomic with its first turns. `TenantCreate = 6` does not advance provenance chains.
The first tenant transaction allows 1023 turns. Later transactions allow 1024.
The tape's 4 MiB encoded batch limit includes headers, salts, and the footer.
That limit can reject content below the codec's independent 4 MiB field limit.

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
u8 trust: trusted=0, untrusted=1
u8 key_present: 0 or 1
if present: idem_key length + UTF-8, <=128 bytes
```

Unknown versions, invalid tags, malformed UTF-8, trailing bytes, and oversized fields are rejected.

## Bounds and cold pages

Page capacities are 16, 64, 256, 1024, then repeated 4096-slot pages.
Arithmetic maps sequences to page slots. Slots store LSNs, not segment offsets.

`EngineOptions` bounds resident slots, sessions, page metadata, tenants, and retry tables.
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
`checkpoint()` waits for an explicit worker snapshot.
Normal checkpoints write only dirty pages, newly registered tenants, and retained retry-key additions.
Deterministic oldest-first eviction rebuilds the retry window without tombstones.
Snapshots share immutable page buffers and retry entries through reference counts.
Appending to a shared page uses copy-on-write.
Cold reads run outside the state lock, then recheck the current backing location before installation.
Compaction can replace or unlink a captured location. Loading retries against the replacement.
File writes and syncs also run outside the state lock.

Full snapshots still enumerate page and retry-entry metadata under the state lock.
That work scales with configured directory bounds, not dirty bytes.
It can delay writer preparation and publication for large directories.
`checkpoint_snapshot_lock_ns` reports the longest snapshot hold, excluding lock acquisition.
The `full_snapshot_shares_pages_and_measures_its_lock_hold` test measures a 1000-page full snapshot.
This measures hold time without asserting a hardware-dependent latency threshold.

Files are named `ckpt-{lsn}.zdb`. Failed attempts may leave numbered suffixes.
Failures before a rename attempt remove partial data and staging files.
An ambiguous rename keeps the complete file until the next open checks the installed chain.
Open removes unreferenced checkpoint files after acquiring the tape lock.
Every record contains a CRC32C, a total length, and a typed payload.
CRC coverage includes the length and payload.
File headers carry the watermark and transaction.
`CHECKPOINT` names the ordered file chain and its final watermark.

Installation syncs the new file and directory before replacing `CHECKPOINT`.
The replacement uses a synced staging file, rename, then directory sync.
The 9th checkpoint compacts the chain into 1 file.
Old chain files are removed only after installation and backing-location replacement.

Open validates checkpoint CRCs, loads page locations, and replays application records above the watermark.
Corrupt or missing checkpoint data logs a warning and triggers full replay.
Checkpoints are derived data. They never authorize truncating tape records.
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
ZERODB_SIM_SEED=19 cargo test -p zerodb-core --release --features sim --test engine_sim -- --nocapture
```

The default is 500 seeds. `ZERODB_SIM_SEED` selects 1 seed unless `ZERODB_SIM_SEEDS` also sets a count.
Failures print the seed and reproduction command.
Each seed runs 3 crash generations across 48 scopes and 4 tenants.
The oracle checks bytes, dense sequences, retry receipts, and window read counts.
It also checks transaction atomicity, tenant IDs, and corrupt-checkpoint fallback.
Sync failures cover errors before persistence and errors after persistence.
Live reopen preserves failed page-cache writes. Crash reopen uses a captured SimDisk image.

`TapeOptions::inline_sim` disables both worker threads and uses fixed salts.
It is feature-gated and defaults to false. Never enable it for real storage.
Explicit seeded checkpoints replace the wall-clock timer.
Memory-pressure checkpoints and recovery spills also run inline.
Production defaults retain worker threads and OS entropy.
