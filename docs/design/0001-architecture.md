# 0001: zerodb architecture (historical)

Scope superseded by [0003: slim v0.1](0003-slim-v0.1.md).
Its frame v2 and recovery rules replace the storage fields below.
The removed features are historical, not current guarantees.

Status: Revised after review rounds 1 and 2 (Skeptic, Constraint Guardian, User Advocate). Decision log in `0002-decision-log.md`.

## Context

Agents need memory that is fast and does not lose data. Today teams wire Redis, a vector DB and a memory framework together. Research (GitHub issues, HN, Reddit, X) found the same failures again and again:

- Writes report success but never persist.
- "Last N turns" gets slower as a session grows.
- Scoped search misses, or returns another scope's data.
- Two processes sharing one memory file overwrite each other.
- Old and new facts both rank as true.
- Deletes miss transcripts and caches.
- Persistent memory widens prompt injection and poisoning.
- Nobody can reproduce memory benchmarks.

## Goals

1. Speed. Recent-window reads cost O(N returned), never O(history). Durable write ack close to a bare append plus sync on the same hardware.
2. Reliability. An acked durable write survives crash and power loss. No partial transactions. Corruption of acked data is detected, never silently truncated.
3. One binary covers 3 jobs: session log, hybrid recall, cache.
4. Opt-in provenance. Every turn and fact is tamper evident and verifiable offline, per tenant. Cache records have CRC integrity only. Works air-gapped.
5. Small, clean code. Storage and index engine written from scratch. Std collections are fine.

## Non-goals for v0.1

Replication. LLM calls inside the DB. Redis protocol. Graph queries. ANN graphs. Local embedder. FSST. TypeScript SDK. MCP over HTTP. These come later, each behind a benchmark that shows the need.

## Core idea

One append-only, checksummed, hash-chained log (the tape) is the write-ahead log, the data store and the provenance chain. Every other structure is a view rebuilt from the tape. One commit point covers session history, cache state, search visibility and provenance.

Second idea, gated on an experiment: assemble context in a prefix-stable order so inference engines with prefix caching get hits (PrefixLedger, milestone 7, kill gate G3).

## Data model

- **LSN.** Global, dense, monotonic u64, assigned by the writer. Never reused. Never changes, including across compaction.
- **Record id** = `(tenant, lsn)`. Stable forever. Locations are resolved through indexes, never baked into ids.
- **Scope.** `tenant / agent / session`. Scope is a correctness boundary.
- **Turn.** `{tenant, agent, session, seq, role, content, meta, vector?}`. `seq` dense per session, assigned by the writer at write time.
- **Fact.** `{tenant, agent?, key, content, meta, event_time?, source_refs, vector?}`. Key uniqueness is per `(tenant, agent)` or per tenant when `agent` is absent. Never per session. Supersession follows LSN order only. `valid_from` is the commit time. `event_time` is stored but never decides supersession. Forgetting the current fact leaves the key with no current value. The previous fact does not come back.
- **Cache entry.** `{tenant, namespace, key, value, expires_at?}`. `expires_at` is absolute wall-clock time. Cache records are not part of provenance chains. They carry a CRC but a zero chain field.
- **Trust label.** Every turn and fact carries `trust: trusted | untrusted`. With auth on, the token decides the maximum trust a writer may assert. `window` always returns every turn in seq order with its `trust` label. Trust filtering applies only to recall: `search` and `assemble` exclude `untrusted` records written by other agents unless the caller sets `include_untrusted`. A caller always recalls its own agent's untrusted writes. This is the poisoning defense: web content and tool output from one agent never reach another agent's recall by default. Promoting a record to `trusted` appends a new `trust_change` record that references it and is chained like any other write. `zerodb init` generates an owner token that may write `trusted`.
- **Retention.** `retain_for` set per tenant or per agent. A record is expired when `commit_time + retain_for` passed at the time of the read, using the policy record in force at the record's LSN, so lengthening a policy never revives records. Expired turns show in `window` as `{seq, expired: true}`. Compaction removes them, the same path as forget.
- **Commit time.** `max(previous_commit_time + 1ns, wall clock)`. Monotonic, so `as_of` by time maps cleanly to an LSN.

## Operations

| Op | Semantics |
|---|---|
| `append(scope, turns[], idem_key?)` | Atomic batch. Returns `(lsn, seqs)` after durable commit. Dedup scope is `(tenant, session)`. Same key and same payload digest within the dedup window returns the original result. Same key with a different payload returns `IdempotencyConflict`. The effective window is reported in metrics. A key older than the window is treated as new. |
| `window(scope, last=N \| since=seq)` | Ordered turns. O(returned). Forgotten turns appear as `{seq, redacted: true}`. |
| `fact_put / fact_get / fact_history` | Temporal facts. `fact_get` returns the current value. History includes superseded and redacted entries. |
| `cache_set / cache_get / cache_del` | TTL optional. Durability per call. |
| `subscribe(tenant, prefix, from_lsn?)` | Post-commit events carrying LSN. Live delivery is at-most-once. Resume by LSN replays from the tape. |
| `search(filter, text?, vector?, k, as_of?)` | BM25 plus exact vector scan, RRF fused. Exact scope filter. One pinned snapshot LSN per query. Each hit explains itself: `bm25_rank`, `vector_rank`, `rrf_score`, `trust`, validity state and `source_refs`. |
| `forget(selector)` | Returns a forget id. Logically complete at commit. Physically complete after compaction and old snapshot deletion. `forget_status(id)` reports both. |
| `export(tenant, as_of?)` | JSONL at one pinned LSN. That tenant's records and chain only. |
| `verify(tenant?)` | Frames, chain, footers, checkpoint signatures. Reports the unsigned tail and any redactions. |

Filter shapes: `tenant` required. Optional `agent`, up to 64 `session` ids, a commit-time range, and record kinds. Nothing else in v0.1.

### Durability modes

- `durable` (default for every op): ack after sync. Visible after sync.
- `fast` (cache ops only, opt-in per call or per namespace): ack after the write reaches the OS. `cache_get` can return a non-durable value and the response says `durable: false`. Turns, facts and search never see non-durable data. A `fast` write exposes no LSN, no id and no event until it is durable, so a crash that truncates it can never make a handed-out LSN point at other data.

### Error outcomes

Every write error carries an outcome: `committed`, `not_committed`, or `unknown`. `unknown` happens only when a sync fails. Idempotency keys make retry safe in that case.

## Tape

### Files

- Data dir holds `LOCK`, `MANIFEST`, `SUPERBLOCK`, numbered segment files (64 MiB) and checkpoint files.
- Opening takes an exclusive lock on `LOCK` (`File::try_lock`). A second opener gets `DataDirLocked`, naming the holding PID.
- `MANIFEST` lists live segments and the current checkpoint. Replaced by write temp, sync, rename, sync dir.

### Frames

```text
u32 crc32c(everything below)
u32 len | u8 kind | u8 version | u16 flags
u64 txn | u64 lsn | u64 commit_time_ns
u32 tenant_id | [32] tenant_prev_chain | [32] content_digest
payload
```

- The CRC covers the length and header, so a corrupt `len` is caught.
- `len == 0` marks the end of written data in a preallocated segment.
- `content_digest = BLAKE3(salt || content)`. A 16-byte random salt is the first field of the payload. Erasing the payload erases the salt, so a forgotten digest cannot be brute-forced.
- Chains are per tenant: `chain_i = BLAKE3(tenant_prev_chain || canon_i || content_digest_i)`. `canon_i` is a fixed encoding of `kind, version, txn, lsn, commit_time_ns, tenant_id` only. Storage fields (CRC, len, husk flag) are never chained, so a husk verifies exactly like the original. Verifying or exporting one tenant never needs another tenant's records.
- `batch_digest` is a BLAKE3 over the batch's content digests, never over raw payloads.
- A batch footer frame closes each transaction: `{txn, record_count, batch_digest}`. A transaction never spans segments. Max batch: 4 MiB or 1024 records.

### Writer thread

The writer assigns LSNs and sequences, then frames, chains, writes, syncs, and publishes.
Callers compute initial content digests before enqueue.
State-machine preparation may rewrite content. The writer refreshes changed digests before framing.
BM25 and vector updates run on workers.

Group commit is leader-follower. The writer drains the queue, writes every pending batch, syncs every file written since the last sync (a group can span a segment roll), then publishes and acks. No timer. Under low load a batch commits alone.

Publish runs on the writer thread before the ack and does three O(1) installs: the session index slot, the cache map entry, and the current-fact pointer. So `window`, `cache_get` and `fact_get` read their own writes at once with O(returned) cost. Only BM25 and vectors lag behind on workers.

### Durable boundary and recovery

- `SUPERBLOCK` holds the last LSN known synced.
  Installation is requested after a published segment roll, 30 seconds, or 256 MiB, whichever comes first.
  The time and byte thresholds are configurable. The byte counter includes roll padding.
  Installation uses its own write, sync, and rename, outside the acknowledgement path.
  Recovery and clean shutdown also install the durable boundary, without the periodic time limit.
- An acked batch is synced, and sync persists every earlier write to the same file. So a torn frame before an acked frame is impossible unless synced data was damaged.
- Recovery rule:
  - Frames at or below the superblock LSN must all be valid. Any bad frame, an end-of-data marker, or a MANIFEST segment missing from disk before the superblock LSN stops the open with `CorruptionDetected` and the LSN. `zerodb repair` is an explicit offline command.
  - Above the superblock LSN, recovery truncates at the first bad frame or the first transaction without a footer. It reports the truncated LSN range and byte count loudly in the open result and logs.
  - Damage above the last superblock install can be indistinguishable from a torn tail.
    Deferred installation enlarges the rewrite and damage-detection window above the last completed superblock.
    Requests occur within the byte threshold plus one published group.
    Helper scheduling and I/O can extend the completed-install lag. It is not a hard repair-size ceiling.
    Successful open and clean shutdown still install the boundary, including short sessions.
- Recovery replays from the minimum watermark across all views, across segments.
  Checkpoint frequency bounds application replay: every 64 MiB of tape or 60 s.
  Derived `ZCM2` manifests seal every chain file's exact length and complete BLAKE3 digest.
  Monotonic State attempt IDs prevent failed same-LSN saves from reusing checkpoint names.
  Missing or mismatched derived files produce a warning and full tape replay.
  Tape verification still scans every segment on open.

### Segment roll

- Segment preparation creates the next file and sets its configured length, 64 MiB by default.
  It syncs the file and directory before installing the manifest.
- The writer only switches to a segment whose directory entry is already durable. No ack ever depends on an unsynced directory entry.

### Fsync failure

A failed sync poisons the engine. Writes fail with `unknown` for in-flight batches and `not_committed` after. Reads keep working at the last durable LSN. Health reports `degraded`. Recovery needs a restart.

### Tape implementation notes

`WriterCore::enqueue` and `WriterCore::step` drive the same state machine in production and simulation.
`Tape` owns its writer and metadata helper. Dropping it joins both and releases the directory lock.
Simulations prepare requests with `PreparedBatch::with_salts`, then pass them through `WriteRequest::new`.
Production salts come from `/dev/urandom` on caller threads. This entropy source requires Unix.
Tape keeps one entropy handle open. Each caller reads its batch's salts together before hashing.
Storage operations still use `Io`. Simulations supply deterministic salts without accessing OS entropy.

MANIFEST version 2 stores the segment size before its segment identifiers.
Opening rejects a different configured size before recovery writes or syncs anything.
Unshipped version 1 manifests aren't accepted.
SimDisk settles failed writeback once when sync fails, using the seeded sector-tearing model.
Later crashes don't replay those writes over newer durable data.

Record LSNs increase strictly. Footer frames copy their transaction's final record LSN.
Footers have no separate read address. Fast acknowledgements expose no transaction identifier or LSN.
The 4 MiB batch limit includes frame headers, salts, and the footer.
An oversized batch never enters the writer. A configured segment can impose a smaller batch limit.

Segment preparation runs inline. Moving it to a background task remains future work.
Superblock requests run after replies, using the roll, time, and byte triggers above.
Metadata failures poison future writes without changing earlier acknowledgements.
After recovery rewrites and syncs touched files and the directory, open installs any newer recovered boundary before returning.
Clean shutdown drains requests and performs its final sync before installing the boundary without the periodic time limit.
Dropping `Tape` joins that shutdown. Poisoned writers and failed final syncs don't advance the boundary.
Recovery permits roll padding no longer than 4 MiB plus a footer below the durable boundary.
Recovery resumes at the first cut segment and offset.
Recovery rewrites retained bytes above the superblock boundary at their original offsets before syncing each file and directory.
A process restart can retain failed-writeback bytes in clean page cache. Another sync alone doesn't guarantee persistence.
Rewriting re-dirties those pages before recovery publishes them or accepts new writes.
The rewrite covers data written since the last superblock install, plus anything unsynced at a crash.
The default request budget is 256 MiB, including padding, plus one published group and helper lag.
With 64 MiB segments, roll notifications normally trigger sooner.
Repair also covers at most one final segment's unused preallocation.
Slow or blocked helper I/O can exceed the request budget. Acknowledgements do not wait for metadata.
Untouched segments already covered by the superblock require neither rewriting nor another file sync.
Retained zero padding is rewritten too. A failed tail-repair sync can leave cached zeros over durable garbage.
This includes the final segment's unused preallocation, adding at most 1 segment's remaining capacity beyond retained data.
Later segments leave the manifest atomically before their files are unlinked.
Open removes segment files beyond the manifest's last identifier and syncs the directory before recovery.
The sync also runs without visible orphans, so retrying a failed cleanup persists earlier unlinks.
Recovery reports discarded LSNs and removed capacity, including preallocated tails.
An unreadable frame can hide its LSN. The reported range includes every decodable discarded record.

### Disk space

The free-space floor defaults to `min(1 GiB, 5% of total volume bytes)`.
Before preparing each group, the writer budgets encoded bytes, segment reservations and index backing growth against available space.
A clean floor hit returns `DiskFull` with `NotCommitted` and sets read-only. Reads keep serving acknowledged data.
The writer reserves segment and LSN-index backing space before `prepare`, including both in the floor budget.
`LSN-OFFSETS` and `LSN-ROUTES` grow in whole pages through the highest LSN the queued group can assign.
Dirty-page eviction during cold reads writes only within reserved backing space, preserving the bounded cache.
Recovery rebuilds resident index pages before optional backing reservations. Full-disk or space-query failures start read-only without poisoning.
The next write retries outstanding reservations and any unknown default-floor query. The on-disk format is unchanged.
Sparse fallback relies on the floor; APFS and btrfs retain the same allocation caveat as segments.
`Engine::health()` reports this observed state without blocking or querying space.
Only a later write's passing space check clears read-only.
Data-write, sync, and manifest failures still poison the writer. Poisoning requires reopening.
Future compaction may use the reserve. Forget and compaction won't run on the writer thread.

## Compaction and forget

- Compaction copies live records from old segments into new ones, oldest segments first.
- A dropped chained record (turn or fact) leaves a husk: canonical header fields plus `content_digest`, no payload. Husks keep every per-tenant chain verifiable after compaction. Cache records are not chained, so compaction drops them with no husk. Steady-state disk under cache churn is live data plus a fixed overhead.
- Husk ranges below a signed checkpoint can be collapsed into one range record holding the chain heads at both ends. `verify` then reports that range as "verified at compaction LSN X", never as fully verified.
- Order: write new segment, sync, sync dir, write checkpoint pointing at it, update `MANIFEST`, then delete old segments. A crash at any point recovers to either the old or the new state. SimDisk tests every point.
- Tombstones (`cache_del`, superseded facts past retention) are dropped only by a pass that covers every older segment. An acked delete never resurrects.
- **Forget** appends a redaction record. From its LSN, every view hides the content. Compaction then rewrites affected segments as husks and rebuilds affected checkpoints without the content. Old checkpoint files, old segments and expired cache values are deleted. Then `forget_status` reports physically complete.
- BM25 document frequencies and vector statistics are recomputed in the next checkpoint after a forget.
- Docs state what forget cannot guarantee: filesystem and SSD remanence (copy-on-write filesystems, flash wear leveling) and exports already taken.
- Facts whose `source_refs` point at forgotten turns stay valid and are flagged `source_redacted`.

## Visibility and snapshots

- The durable LSN is the highest LSN synced. Every view, the delta scan and pub-sub expose nothing past it. The only exception is `fast` cache values, marked `durable: false`.
- Each query pins `snapshot_lsn = durable_lsn` at start, or the requested `as_of`. BM25, vectors, the delta scan and the allow-set are read as of that LSN. The forget and retention sets are always read at the current durable LSN, so `as_of` never resurrects forgotten content.
- Each index has a watermark. Queries scan the delta between the watermark and the snapshot. Index lag is capped (default 64 MiB or 100K records). At the cap, `append` waits up to 5 s, then fails with `IndexLagExceeded` (`not_committed`). A dead or stuck worker marks the engine `degraded`. Lag is a metric.

## Session index

- `(session, seq)` maps by arithmetic to a slot in a paged offset vector. Each slot holds `(segment u32, offset u32)`.
- Pages grow geometrically: 16, 64, 256, 1024, 4096 slots. A 3-turn session costs 128 bytes, not 32 KiB.
- Hot pages share a global memory budget with LRU eviction. Cold pages persist in checkpoints and load lazily.
- Compaction updates slots in the next checkpoint, and in place for hot pages under a short lock.

## Cache

- Std `HashMap` keyed by `(tenant, namespace, key)`. TTL via a `BinaryHeap` of `(expires_at, key)`.
- Expiry writes no tape record and emits no event. Reads check `expires_at`. Recovery drops expired entries. Compaction drops their bytes.
- Bounded by `cache_max_bytes` per tenant. At the bound: `reject` (default, typed `ResourceExhausted`) or `evict_lru`, set per namespace.
- Clock: wall clock. A backward clock step can extend a TTL and a forward step can shorten one. Documented. Simulation uses `SimClock`.

## Pub-sub

Events are emitted after commit, carry an LSN, and go to a bounded per-subscriber buffer. On overflow the subscriber gets `Lagged { resume_from_lsn }` and is dropped. The writer is never blocked. `subscribe(from_lsn)` replays from the tape, then switches to live.

## Search

- Tokenizer: versioned, Unicode lowercase, split on non-alphanumeric.
- BM25: mutable recent postings plus sealed postings in checkpoints. Plain evaluation.
- Vectors: client-supplied f32 at write time. Exact scan per tenant partition, SIMD-friendly layout. Recall is exact, so G2 measures latency at scale. IVF is milestone 5, built only if exact scan misses the latency target at the documented scale.
- Visibility: text search, window and fact_get see a write at its durable LSN. Vector search sees it at the same LSN, because vectors arrive with the write. A turn without a vector is invisible to vector-only queries, and the response counts how many scoped records had no vector.
- Fusion: RRF, `c = 60`.

## Provenance

- Per-tenant chains are always computed.
- Signed checkpoints are opt-in. The engine signs `(tenant_id, lsn, chain_head, commit_time, key_id)` with Ed25519 every N records or T seconds, and on clean shutdown.
- The signing key lives in a separate file path, ideally on another volume or injected at start. Docs state the claim exactly: tamper evidence relative to whoever holds the signing key.
- `verify` reports the unsigned tail (records after the last signed checkpoint).
- Writer identity is client-asserted metadata unless the server binds it from the auth token.

## Server and MCP

- HTTP/JSON server binds `127.0.0.1` by default.
- Auth is on by default. API tokens in config map to a tenant, optionally an agent, and a maximum trust level. Tenant and agent come from the token, never from the request body.
- `--insecure-no-auth` is for local dev only. It requires a loopback bind and rejects requests whose `Host` or `Origin` is not loopback, which blocks DNS rebinding from browsers.
- MCP stdio adapter. If the data dir lock is held, it connects to the running server over HTTP (`ZERODB_URL` or the address recorded in `LOCK`) with a token from `ZERODB_TOKEN`. Otherwise it fails with a clear message. It never opens the data dir directly. MCP writes are `untrusted` unless the token allows more.
- MCP scope comes from adapter config, never from tool arguments. Tools: `memory_append`, `memory_window`, `memory_search`, `fact_put`, `fact_get`, `cache_get`, `cache_set`. `forget` is off unless enabled in config. Durability is fixed to `durable`.

## Deterministic simulation

- All I/O goes through an `Io` trait and all time through a `Clock` trait.
- The engine core is a state machine. In tests a seeded single-threaded scheduler drives the writer, workers and clients, so a seed reproduces a failure exactly.
- `SimDisk` faults: lost, reordered or torn unsynced writes. Sync failure. Unsynced directory entries. Rename crash states. Bit flips in synced data. ENOSPC partial writes.
- Invariants checked after every simulated crash:
  - every acked durable write is present;
  - no partial transaction is visible;
  - every view equals a rebuild from the tape;
  - no forgotten content is readable;
  - every chain verifies.
- CI runs a fixed seed budget on every commit, and a larger one nightly.
- The scheduler replaces real channels and wakeups, so a separate real-thread stress test covers the writer queue, leader-follower ack wakeups and in-place slot updates.
- G1 also includes a real-filesystem crash test with LazyFS on Linux, nightly.

## Memory bounds

Every in-RAM structure has a configured bound and a typed `ResourceExhausted` error.

| Structure | Bound (default) | At the bound |
|---|---|---|
| Hot session pages | 256 MiB total, LRU | Evict to cold pages |
| Cache values | `cache_max_bytes` per tenant (64 MiB) and 1 GiB total | `reject` or `evict_lru` per namespace |
| Vectors in RAM | 512 MiB per tenant, 4 GiB total | Writes with vectors rejected |
| Mutable BM25 postings | Flushed to sealed postings at 64 MiB | Flush |
| Dedup table | 1M entries per tenant, 8M total | Oldest entries dropped |
| Tenants | 10,000 | `ResourceExhausted` on new tenant |
| Per-tenant chain heads | 72 bytes per tenant, bounded by tenant count | n/a |
| Subscriber buffers | 1,024 events each | `Lagged`, subscriber dropped |

## Dependencies

Written here: tape, frames, CRC32C, SimDisk, scheduler, session index, BM25, vector scan, RRF, compaction, recovery.

Allowed crates, each with a reason:
- `blake3`, `ed25519-dalek`: audited crypto, never hand-rolled.
- `serde`, `serde_json`: API encoding.
- `tokio`, `axum`: HTTP server only. The engine core does not use async.
- `proptest`, `criterion`: tests and benches.

S2 replaces the original no-libc rule with rustix for free-space queries and preallocation. D71 records the choice.
Segments and LSN-index backing files are allocated on the write path before application preparation.
Linux and macOS reserve blocks before setting the file length. Unsupported operations fall back to sparse `set_len`.
Other platforms also use sparse `set_len`. Preallocation isn't a guarantee on APFS or btrfs.
Rust std `sync_data` already issues `F_FULLFSYNC` on macOS.

## Milestones and size budgets

| M | Scope | Gate | Rust LOC budget (excl. tests) |
|---|---|---|---|
| 1 | Io, SimDisk, Clock, frames, tape writer, recovery, superblock, manifest, lock | SimDisk invariants | 3K |
| 2 | Session index, append, window, idempotency, incremental checkpoints (bounded I/O per interval) | G1 | 2K |
| 3 | Cache, TTL, pub-sub, facts | SimDisk invariants | 2K |
| 4 | Compaction, forget, retention, husks, provenance, verify, export | SimDisk + forget invariant | 3K |
| 5 | BM25, exact vector scan, RRF, snapshot LSN | G2 | 2.5K |
| 6 | HTTP server, auth, MCP stdio, Python SDK, markdown import and export | API tests | 2.5K |
| 7 | PrefixLedger | G3 | 1.5K |

## Kill gates

- **G1:** 1M-turn trace with mixed cache writes and concurrent reads, on Linux with NVMe. Durable-ack p99 within 1.5x a bare append-plus-`sync_data` baseline using the same preallocation, record sizes and batching. Zero acked loss and zero partial transactions across the SimDisk budget and the LazyFS run. Also reported on macOS, with no pass threshold.
- **G2:** Scoped recall@10 equals 1.0 for exact scan. Zero filter violations. p99 search under 20 ms at 100K vectors (768 dims) per tenant on one core, measured with index lag held at the cap. If missed, M5 adds IVF and the gate becomes recall@10 at least 0.95.
- **G3:** LoCoMo replay. Median TTFT at least 30% lower with prefix-stable assembly and no accuracy loss. Failure drops PrefixLedger.

## Evaluation contract

Every published number pins the dataset, its revision, models, judge, top-k and token budget, zerodb version, and hardware, and ships raw outputs in `bench/`. Storage latency is reported apart from embedding and answer generation. Baselines always include a plain markdown wiki and SQLite, because users report those beat memory frameworks.

## Milestone carry items from review

- M1, M4: recovery and verify accept LSN gaps from dropped cache records, and still require a husk for every chained record.
- M4: without signing, husks cost about 100 bytes per dropped record forever. Unsigned collapse is allowed and `verify` reports it as unverified. Ids into collapsed ranges resolve to `collapsed`.
- M2: the dedup window shrinks as tenants grow. Metric reports it.

## Risks

- Open remains O(tape) for CRC, digest, and chain verification, even with a current checkpoint.
  Checkpoints bound application replay, not tape verification.
  A later milestone must add verified-segment markers before we claim checkpoint-bounded open time.
- Superblock window: up to 1 s of synced bit rot looks like a torn tail. Accepted and documented.
- Single writer caps write throughput per data dir. Sharding by tenant comes later.
- Exact vector scan cost grows linearly per tenant. IVF is the planned answer.
- Wall-clock TTL. Accepted and documented.
