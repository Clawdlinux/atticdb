# zerodb M2: session log. Implementation plan

> For agents: subagent-driven development. One implementer per group, then spec review, then quality review. TDD for every task.

**Goal:** turns go in with `append` and come out with `window`, durably, in O(returned), with safe retries and bounded recovery time.

**Design refs:** `docs/design/0001-architecture.md` sections Data model, Operations, Writer thread (publish step, N5), Session index, Visibility, Memory bounds. Decisions D2, D16, D19, D20, D28, D29, D38, D44, D49, D51.

**Crates:** M1's `zerodb-tape` gains two small hooks. New crate `crates/zerodb-core` holds the engine. No new third-party deps.

**Budget:** about 2K production lines for M2.

---

## Group D: tape hooks

### D1. State machine hook in the writer

The writer must assign session seqs at write time (D19) and install index entries before the ack (N5, D44). Tape stays generic, so add a trait:

```rust
pub trait StateMachine: Send {
    /// Writer thread, before framing. May rewrite payloads (assign seqs),
    /// reject the batch, or short-circuit it with a cached reply (idempotency).
    fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare;
    /// Writer thread, after sync, before any reply. Must be O(records).
    fn publish(&mut self, committed: &[CommittedRecord]);
    /// Recovery replay, in LSN order, for records above the checkpoint watermark.
    fn replay(&mut self, record: &CommittedRecord) -> Result<()>;
}
pub enum Prepare { Write, Reject(WriteError), Reply(Committed) }
```

- `CommittedRecord { lsn, txn, kind, tenant_id, commit_time_ns, payload: Arc<[u8]> }`.
- `prepare` runs once per batch in queue order, so seq assignment follows commit order.
- A batch that later fails its sync: `prepare` already advanced in-memory seq counters. The writer is poisoned at that point, so no later batch can observe them. Document this.
- `Tape::open_with(state_machine, ...)` and `WriterCore` generic over `S: StateMachine`. The existing no-op path stays for tape-only tests (`NoopStateMachine`).
- Tests: seq assignment order under 8 threads; publish before reply (reply observed only after publish ran); a rejected batch writes nothing; `Reply` writes nothing and returns the cached result.

### D2. Tape scan

- `Tape::scan(from_lsn) -> impl Iterator<Item = Result<CommittedRecord>>` over durable records, segment by segment. Footers and husks skipped.
- Used by recovery replay and tests. Bounded memory: one frame at a time.
- Tests: scan from 0, from the middle, from past the end; scan after a roll; scan never passes `durable_lsn`.

---

## Group E: engine, session index, append and window

### E1. Turn payload codec

- `crates/zerodb-core/src/turn.rs`. Versioned binary layout, little-endian: `u8 version | agent_id(len-prefixed utf8, max 255) | session_id(len-prefixed, max 255) | u64 seq | u8 role | meta(len-prefixed, max 64 KiB) | content(len-prefixed, max 4 MiB) | u8 trust | idem_key?(len-prefixed, max 128)`.
- `role`: system, user, assistant, tool. `trust`: trusted, untrusted.
- Seq is written as 0 by the caller and set by `prepare`.
- Tests: round trip (proptest), every length limit rejected, garbage never panics.

### E2. Tenant registry

- `tenant` strings map to `u32 tenant_id` through a `TenantCreate` record on the tape (new frame kind 6, unchained, payload = tenant name). Max 10,000 tenants (memory bounds table).
- Created on first append, inside the same batch, before the turns.
- Tests: ids stable across reopen; limit returns `ResourceExhausted`.

### E3. Session index

- `crates/zerodb-core/src/session.rs`. Session key `(tenant_id, agent, session)` maps to `SessionEntry { next_seq, pages }`.
- Pages hold `u64 lsn` slots, geometric sizes 16, 64, 256, 1024, 4096. Seq to slot by arithmetic. Locations resolve through LSN (D16), using the tape's existing LSN map.
- `window(last = N)` and `window(since = seq, limit)` read slots then `Tape::read(lsn)`. Cost O(returned).
- Global hot-page budget (default 256 MiB). Over budget, least recently used session pages are dropped from RAM and reloaded from the checkpoint plus replay. M2 may implement reload as "rebuild that session's pages by scanning its checkpointed page file", see E5.
- Tests: dense seqs; window last/since on 1, 16, 17, 5,000 turns; cost independent of history (count `Tape::read` calls, not time); budget eviction and reload.

### E4. Idempotency

- Dedup scope `(tenant_id, session, idem_key)`. Value: `(payload digest of the batch, Committed)`.
- Bounded per tenant (default 1M) and globally (8M). Oldest dropped first. A metric (`dedup_window_effective`) reports the smallest per-tenant window.
- In `prepare`: same key and digest returns `Prepare::Reply`; same key, different digest returns `IdempotencyConflict` with `NotCommitted`.
- Rebuilt on replay from the idem_key stored in turn payloads.
- Tests: retry returns the original seqs and LSNs and writes nothing; conflict; cross-session same key is independent; cross-tenant isolation; eviction bound; survives reopen.

### E5. Checkpoints (incremental)

- Every 64 MiB of tape or 60 s, a worker thread writes dirty session pages and the dedup table delta to `ckpt-{lsn}.zdb` (CRC-framed records), then installs `CHECKPOINT` (atomic install) naming the file chain and the watermark LSN.
- Checkpoint I/O per interval is bounded by dirty data (N12). A full compaction of checkpoint files happens when the chain exceeds 8 files.
- Open: load the checkpoint chain, then replay the tape from the watermark through `StateMachine::replay`.
- Tests: open time bounded (replayed records count ≤ records since last checkpoint); crash during checkpoint install at every SimDisk point leaves the previous checkpoint valid; corrupt checkpoint file falls back to full replay with a logged warning (checkpoints are derived data, never authoritative).

### E6. Engine API

```rust
pub struct Engine { ... }
impl Engine {
    pub fn open(path, EngineOptions) -> Result<Engine>;
    pub fn append(&self, scope: &Scope, turns: Vec<NewTurn>, idem_key: Option<&str>) -> Result<Appended, EngineError>;
    pub fn window(&self, scope: &Scope, q: WindowQuery) -> Result<Vec<Turn>, EngineError>;
}
```

- `EngineError` carries `Outcome` (committed, not_committed, unknown) per D38.
- Read-your-writes: `window` right after `append` returns the turn (publish runs before the ack).
- `window` never returns records above `durable_lsn`.

---

## Group F: simulation, G1 harness

### F1. Engine simulation

- Extend the M1 sim: drive `Engine` (single-threaded core) on SimDisk with random appends across sessions, idem retries (including after `Unknown`), windows, checkpoints, crashes and live reopens.
- Oracle: per session, acked turns form a dense prefix by seq with identical bytes; `window` results equal the oracle; an idem retry never creates a duplicate.
- 500 seeds in CI, 10,000 once locally.

### F2. G1 harness

- `crates/zerodb-core/benches/g1.rs` plus `bench/g1.sh`: 1M turns, mixed sizes, 16 writers, concurrent `window` readers. Compare durable-ack p99 against the M1 bare baseline with the same record sizes and batching.
- Run on macOS now and record. The real G1 verdict needs Linux with NVMe; RESULTS.md says so plainly.

---

## M2 acceptance

- All tests green, fmt and clippy clean, 10,000 engine sim seeds pass once.
- `window` cost proven independent of history by call counting.
- G1 harness numbers recorded for macOS. Linux NVMe run listed as pending.
- Production lines for M2 under 2K.
