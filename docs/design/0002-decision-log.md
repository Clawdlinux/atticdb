# 0002: zerodb decision log

Round 1 review: Skeptic, Constraint Guardian, User Advocate (Claude Opus 5.5). Designer and arbiter: lead agent. Objection ids match the review.

| Id | Decision | Alternatives considered | Why |
|---|---|---|---|
| D1 | Tape is WAL, store and provenance chain at once | Separate WAL plus store; LSM | One commit point. Fewest moving parts. Research: single append file with group commit is the simplest durable path. |
| D2 | Session index by arithmetic over paged offset vectors | B-tree, skip list, learned index | Dense seqs make lookup arithmetic. O(returned) reads. |
| D3 | v0.1 vectors are exact scan, IVF is gated | HNSW, IVF-flat, RaBitQ | Exact recall, no drift, little code. Add IVF only if G2 latency fails. (S13, C10) |
| D4 | Scope cut: no local embedder, FSST, TS SDK, MCP HTTP, custom hash table or timing wheel in v0.1 | Ship everything in v0.1 | Every component must serve G1 or G2. "From scratch" means storage and index code, not std collections. (S13, C10) |
| D5 | Forget is logical at commit, physical only via compaction into husks | In-place zeroing; rewrite CRC in place | Sealed segments never change. No crash state can refuse open. (S1) |
| D6 | Salted content digest `BLAKE3(salt or content)`, salt erased with payload | Unsalted digest; keyed digest with external key store | Forgotten low-entropy content cannot be brute-forced. No extra key store. (C5) |
| D7 | Per-tenant hash chains | One global chain | Tenant verify and export never touch other tenants. (C5) |
| D8 | SUPERBLOCK durable LSN, strict below, truncate above | Strict everywhere; truncate everywhere; sync superblock per commit | Correct under reordered unsynced writes. Bit rot window bounded to about 1 s. No extra sync on the ack path. (S2) |
| D9 | CRC covers length; `len == 0` ends data | CRC over payload only | Corrupt length detected. Preallocated tails handled. (S2) |
| D10 | Pre-created segments with durable dir entries; MANIFEST via rename | Create on roll | No ack depends on an unsynced directory entry. (S3) |
| D11 | Compaction order: new segment, sync, dir sync, checkpoint, manifest, delete | Delete first; swap pointers in memory | Every crash point recovers to old or new state. (S3) |
| D12 | Visibility bounded by durable LSN; one pinned snapshot LSN per query | Read page cache tail; per-index watermarks only | No reader ever sees a write that a crash removes. Consistent fusion. (S4, S9) |
| D13 | `fast` mode only for cache ops, marked `durable: false` | Allow fast turns; keep `group` mode | Cache UX like Redis when asked, never for history or search. `group` merged into `durable`. (S4, U3) |
| D14 | Recovery replays from minimum watermark across all views; checkpoints every 64 MiB or 60 s | Replay active segment only | Bounded open time, no missed records across rolls. (S5) |
| D15 | Index lag cap back-pressures append | Unbounded lag | Bounded delta scans. (S5, C1) |
| D16 | Record id `(tenant, lsn)`; locations resolved through indexes | `(segment, offset)` ids | Ids survive compaction. (S6) |
| D17 | Husks keep header and digest of dropped records | Per-segment anchors | Chain stays verifiable after compaction. (S6) |
| D18 | Tombstones dropped only by a pass covering all older segments | Drop when compacted | Acked deletes never resurrect. (S6) |
| D19 | Writer assigns LSN and seq at write time; txn never spans segments; max batch 4 MiB or 1024 records | Assign at enqueue | Dense seqs, simple recovery. (S7) |
| D20 | Idempotency keys with a bounded dedup table | At-least-once only | Safe retry after `unknown`. (S7, U4) |
| D21 | Fact supersession in LSN order; key scope tenant or tenant/agent; commit time is valid_from | Client time; session-scoped keys | Deterministic history. Cross-session preferences work. (S8, U5) |
| D22 | Forgetting the current fact leaves no current value | Revive previous fact | Never resurrect data the user removed. (S8) |
| D23 | Pub-sub post-commit, at-most-once live, resume by LSN, bounded buffers, `Lagged` on overflow | Durable queues; blocking delivery | Writer never blocks. Crash recovery by replay. (S10) |
| D24 | Wall-clock TTL, documented; SimClock in tests | Monotonic-only TTL | TTL must survive restart. Clock skew risk accepted. (S11) |
| D25 | Seeded single-threaded scheduler drives the core in tests; added fault classes; LazyFS nightly | Real threads under SimDisk | Reproducible failures. Real filesystem coverage. (S12) |
| D26 | Writer thread does framing, CRC, chain, write, sync only; digests on caller threads | Index on writer | Keeps G1 honest on fast-sync hardware. (C1) |
| D27 | G1 on Linux NVMe vs bare `sync_data` baseline with same preallocation | Any hardware | Slow disks hide overhead. (C1) |
| D28 | Geometric session pages with a global hot-page budget | Fixed 4096-slot pages | 128 bytes for a 3-turn session instead of 32 KiB. (C2) |
| D29 | Every in-RAM structure bounded; typed `ResourceExhausted` | Unbounded | No OOM that kills every tenant. (C3) |
| D30 | Forget reports logical and physical completion; recomputes stats; docs list remanence limits | Claim full erasure | Honest. Covers snapshots, old segments, caches. (C4, U6) |
| D31 | Server binds localhost; token maps to tenant and agent | Tenant in request | Blocks cross-tenant reads, including via prompt injection. (C6, U7) |
| D32 | Signed checkpoints carry key id; verify reports unsigned tail; claim stated relative to key holder | Implicit trust | Honest provenance claim. (C7) |
| D33 | Disk reserve, `DiskFull` before reserve, compaction and forget off the writer thread | Fill disk then fail | No compaction deadlock. (C8) |
| D34 | No `libc` in v0.1 | Allow `libc` for fallocate | `set_len` and std `sync_data` suffice. (C9) |
| D35 | Per-milestone LOC budgets | No budget | Keeps code small. (C10) |
| D36 | Exclusive data dir lock; MCP stdio proxies to the running server | Advisory only | Directly fixes the MCP multi-process data loss. (U1) |
| D37 | Visibility stated per channel; response counts records without vectors | Single blanket claim | No hidden gaps. (U2) |
| D38 | Write errors carry `committed`, `not_committed` or `unknown`; poisoned engine serves reads | Generic errors | Clients know when to retry. (U4) |
| D39 | Window shows redacted placeholders; facts with forgotten sources flagged | Skip seqs silently | Predictable output. (U6) |

## Round 2 (review of revision)

| Id | Decision | Why |
|---|---|---|
| D40 | Sync every file written since last sync before ack | Group can span a roll. (N1) |
| D41 | End of data or missing segment below superblock LSN is corruption | No silent loss of synced data. (N2) |
| D42 | Chain over canonical header fields only; husks keep them | Husks verify. (N3) |
| D43 | Cache records unchained, dropped by compaction with no husk; husk ranges collapse below signed checkpoints | Bounded disk under cache churn. (N4) |
| D44 | Publish installs session slot, cache entry, current-fact pointer before ack | Read-your-writes and O(returned). Only BM25 and vectors lag. (N5) |
| D45 | Forget and retention sets read at current durable LSN for `as_of` | No resurrection. (N6) |
| D46 | `fast` writes expose no LSN, id or event until durable | LSNs never reused in any visible way. (N7) |
| D47 | Auth on by default; no-auth needs loopback plus Host and Origin checks | Blocks local and DNS-rebinding access. (N8) |
| D48 | Lag wait max 5 s then `IndexLagExceeded`; dead worker sets `degraded` | No hung appends. (N9) |
| D49 | Idempotency scope `(tenant, session)`, payload digest check, count-based window | No cross-tenant replay. (N10) |
| D50 | Real-thread stress test besides the simulated scheduler | Covers production concurrency code. (N11) |
| D51 | Incremental checkpoints with bounded I/O per interval | Checkpoint cost stays flat. (N12) |
| D52 | Memory bounds table for every structure | Closes C3. |

## Evidence-driven additions (Reddit and X synthesis)

| Id | Decision | Why |
|---|---|---|
| D53 | `trust` label per record; untrusted excluded from recall by default; token caps trust | Persistent-memory poisoning threads. |
| D54 | Per-namespace `retain_for` on turns and facts | Users want memory that expires. |
| D55 | Search hits explain ranks, trust, validity and sources | Opaque memory is hard to debug. |
| D56 | Markdown import and export in M6 | Markdown wikis beat memory frameworks in a 2,176-task test users cite. |
| D57 | Evaluation contract with markdown and SQLite baselines | Benchmarks people can reproduce. |

## Round 3 (arbiter)

| Id | Decision | Why |
|---|---|---|
| D58 | `window` never filters by trust; recall excludes other agents' untrusted writes; own writes always recalled; `trust_change` records promote | Keeps O(returned), valid transcripts and first-use round trip. (B1) |
| D59 | `zerodb init` issues an owner token allowed to write `trusted` | Plug-and-play default. (B1) |
| D60 | Retention per tenant or agent, policy at record LSN, `expired` placeholders | No namespace on turns, no revival. |
| D61 | Goal 4 narrowed to turns and facts | Cache is CRC only. |

Arbiter disposition: APPROVED once D58 to D61 landed, with carry items listed in 0001.

## Rejected

- Dependency invalidation of derived facts (expire facts when sources change): deferred to after v0.1. Facts already flag `source_redacted`. Full invalidation needs a dependency graph, which is not needed for G1 or G2.

## Slim v0.1 (0003)

| Id | Decision | Why |
|---|---|---|
| D62 | Remove hash chains, salted digests, husks and signing | No user evidence asked for tamper evidence. They complicated compaction and forget |
| D63 | Remove Fast durability, Cache kind, pub-sub, trust labels from v0.1 | No evidence. Fast mode broke "acked means durable" |
| D64 | Frame v2: 40-byte header, location-seeded CRC32C | Same integrity class as SQLite and Postgres, without cryptography |
| D65 | Footer binds `first_lsn`, `record_count`, `records_crc`, `synced_lsn` | Atomicity without batch digests. `synced_lsn` proves corruption above the superblock without a counter |
| D66 | Dense record LSNs and txns; footers consume no LSN | Simple recovery checks and paged LSN arrays |
| D67 | Every write carries an `idem_key` | Safe retries over HTTP and MCP, including forget and fact delete |
| D68 | Facts scoped per tenant, prefixes for agent-local keys | Avoids split-brain facts between agents |
| D69 | Forget is "hide, not erase" with an LSN cutoff | Evidence asks to stop recall, not forensic erase. Honest naming |
| D70 | Markdown import deferred | Direct fixes cover the evidence |
| D71 | `rustix` allowed for preallocation and free space | Replaces D34. Needed for disk-full safety |
| D72 | Build in steps S1 to S6, each with two independent reviews and real use | Avoid designing ahead of evidence |
| D73 | Operator truncation accepts 0 or a whole transaction boundary. Interior cuts name both nearest boundaries and fail. | I2 forbids partial transactions. Never round silently. |

D73 recovery details:

- Validate every retained transaction. Report the first bad LSN as `CorruptionDetected`.
- Persist a CRC-protected truncation intent before changing data.
- Truncate, zero and sync data before lowering and syncing the superblock.
- Replay an unfinished intent at open. A crash between those steps must recover.
- Report and log the cut LSN, dropped LSN range and dropped byte count.
- Dropped LSNs may be reused, even when acknowledged. This operator escape is the documented exception to I3.
- Recover `commit_time` from the retained tape. It stays monotonic from that point.
