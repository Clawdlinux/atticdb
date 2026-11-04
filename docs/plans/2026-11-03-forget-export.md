# S5: forget and markdown export. Implementation plan

**Status, 2026-11-03: done. Shipped in #165, #166, #167, #168, #169 and the Task 6 docs PR. Supersedes the 2026-10-30 plan** (`docs/plans/2026-10-30-forget-export.md` on branch `docs/s5-plan`, commit `6176d3d`, written at `239107f`). That file stays on its branch for history. Issue #45. Studio forget is the owner ask of 2026-11-03. It replaces the old plan's "no studio forget button". Base: `main` at `51271cc`.

> For agents: 1 implementer per task. Each task is 1 PR from `main`, built TDD. Before merge it gets a spec review, a code-quality review and a Copilot review. A task branches from `main` only after every task it depends on is merged to `main`. Branching from an unmerged task branch doesn't count. Tasks 2 and 3 may run in parallel once Task 1 is merged. Tasks 4 and 5 may run in parallel once Task 3 is merged. See "Order".

**Goal:** A tenant hides a session, a fact key (file refs included) or a list of LSNs with 1 durable, idempotent `Forget` write. After the ack, no normal AtticDB read returns the hidden content (I10). That includes the hot cache, the word index and its trigram lookup, compressed turns and file refs. Names, keys, LSNs, counts and sizes stay observable. "What a forget leaves visible" lists them. Users forget from the CLI and, when started with `--allow-forget`, from studio. Studio selects sessions in bulk, forgets a fact key, shows placeholders and asks first. A tenant exports its memory as markdown at 1 durable LSN, over HTTP and with `atticdb export <dir>`. Forget hides. It does not erase.

**What changed on main since 2026-10-30.** Each item is folded into the tasks.
- #125 renamed zerodb to AtticDB. The crates are `atticdb`, `atticdb-core`, `atticdb-tape` and `atticdb-sys`. D89 and D90 now hold the rename and the release workflow, so the old D89 and D90 become D98 and D100.
- #147 file refs: a ref is the fact `file<absolute path>` (D94). Forgetting a ref is a fact-key forget.
- #153 turn compression: version 2 turn payloads, raw deflate (D95).
- #154 word index (D96) and #155 read path: trigram token lookup, hot cache, shared segment handles (D97). Search has its own walk and no longer shares `newest_first` with `records`.
- #156 read benchmark `bench/reads.py`. #158 onboarding: `setup` and `doctor`. Studio's chat-start preview reuses the hook's reader.
- The MCP tool logic moved from `mcp.rs` to `client.rs`. The CLI verbs call `client::command`.
- The 7 changes from the 2026-10-30 design review are now inside the tasks: engine write and readers in 1 PR, `HashSet` forget sets, conditional writes that see forgets, export that budgets encoded bytes, the CLI as friction rather than approval, more boundary rows, and a new reader audit (below).

**Facts this plan relies on (checked 2026-11-03 at `51271cc`):**
- Design:
  - `Forget` is kind 3. Its selector is a session, a fact key or an LSN list, inside 1 tenant (0003:55). Its retry scope is the tenant (0003:117, I6).
  - A forget covers records at or below its own LSN, or at or below `up_to_lsn` (0003:121 I10, 0003:177 S5).
  - Placeholders are `{seq, lsn, redacted: true}`, with O(1) forget checks (0003:116 I5). D39 chose placeholders over skipped seqs (0002:45). 0004:91 relies on this shape too.
  - Forgetting the current fact leaves no current value (D22 at 0002:28, 0003:120 I9).
  - Forget sets are read at the current durable LSN, so an old snapshot never brings content back (D45 at 0002:56, 0001:223).
  - Docs must state remanence and exports already taken (0001:217, D30 at 0002:36). Retry digests in checkpoints are unsalted BLAKE3 (0003 S5).
  - 0001 kept `forget` off in MCP unless config enabled it (0001:266). Forget is "hide, not erase" (D69 at 0002:101). Every write carries an `idem_key` (D67 at 0002:99).
  - New: studio is read-only and "never a write route" (D87 at 0002:119). Task 4 amends that with D99.
  - New: D88 (0002:120) adds no CLI allow rules and redacts secrets before writes. D94 (0002:126) file refs. D95 (0002:127) compression, with the retry digest over version 1 bytes. D96 (0002:128): the index only proposes candidates, and the read check decides. D97 (0002:129): hot cache, CRC-checked on every hit (0007).
  - Decision rows D94 to D97 use 4 cells: decision, alternatives, why. New rows follow them.
- Codec and tape:
  - `Kind::Forget = 3` (`atticdb-tape/src/frame.rs:24`). Unchanged.
  - `Forget` has `selector`, `idem_key` (at most 128 bytes) and `up_to_lsn` (`atticdb-core/src/forget.rs:22-31`). Limits: agent and session 255 bytes, key 1,024 bytes, 1,024 LSNs (`forget.rs:7-20`, `forget.rs:34-56`). The doc at `forget.rs:22` says "The engine does not apply forgets yet".
  - The tape sim writes `Forget` frames with random payloads and never decodes them (`atticdb-tape/src/sim.rs:667`). Unchanged.
  - New: `turn::decode` inflates version 2 content (`atticdb-core/src/turn.rs:153`). `turn::decode_ref` parses agent, session and seq without inflating (`turn.rs:256`).
- Engine (`atticdb-core/src`):
  - Prepare treats every non-fact batch as turns (`machine.rs:91`, `machine.rs:113`). Install rejects other kinds as corrupt (`machine.rs:301`, was 288). A unit test pins this (`machine.rs:442`, was 429).
  - Prepare stamps the batch's tenant on every record (`machine.rs:196`, was 189). The tenant comes only from the token (`atticdb/src/api.rs:87-106`).
  - Conditional writes compare `if_lsn` with the newest raw revision (`machine.rs:162-169`). The key cap check is at `machine.rs:170-172` through `Facts::full` (`facts.rs:53-57`).
  - Retry scopes are `Session` and `Fact` (`dedup.rs:12-15`). Checkpoint tags: 1 tenant, 2 page, 3 and 5 retries, 4 fact entry. They load at `checkpoint.rs:201-283` and are written at `checkpoint.rs:528-558`. An unknown tag fails the load (`checkpoint.rs:283`, was 278). Open then falls back to a full replay (`engine.rs:455-482`, was 421-446).
  - Receipts hold txn, LSNs and seqs, never content (`engine.rs:33-44`, `engine.rs:59-66`). A covered retry answers from the dedup table (`start`, `engine.rs:620-649`, was 553-583).
  - `write_fact` is at `engine.rs:679-716` (was 612-649). `enqueue_fact_sim` is at `engine.rs:726-741`.
  - `window_snapshot` (`engine.rs:908-1026`, was 865-924) reads the session under the state lock (`913-931`), then serves each turn from the hot cache (`959-979`) or from 1 tape read, which it then caches (`980-1010`). `Turn` and `Revision` have no redacted flag (`engine.rs:129-142`, `engine.rs:85-98`).
  - `fact_get`, `fact_history` and `fact_list` (`engine.rs:1028-1110`, was 928-1011) pick revisions with `visible` (`facts.rs:16-18`).
  - `search` (`engine.rs:1115-1223`) checks only that a fact is current (`current`, `engine.rs:1125-1130`). Turns match with no other check (`engine.rs:1142-1148`). It walks the newest 64 records and those above the index watermark (`1204-1209`), then the index candidates (`1210-1219`). Every visited LSN counts toward `visits`, and the visit cap's cursor is the next candidate's LSN + 1 (`1187-1202`).
  - `records` (`engine.rs:1228-1274`) walks raw frames. `engine.rs:1229` says "Forgets are not applied yet". Changed: only `records` uses `newest_first` now (`engine.rs:1290-1324`).
  - `sessions` totals count index slots (`engine.rs:1354-1355`, was 1205-1208).
  - `EngineOptions` (`options.rs:35-69`, was 5-29) has no forget cap. New fields: `compression`, `search_index_bytes`, `search_walk_first`, `hot_cache_bytes`.
  - A checkpoint is full once the chain holds 8 files. Others write dirty entries (`checkpoint.rs:343`, `checkpoint.rs:375`, dirty clear at `checkpoint.rs:602-608`).
  - Dirty clearing is conditional today. `save` takes the snapshot under the state lock (`checkpoint.rs:488`), at `snap.lsn = state.published_lsn`. After the files are durable it clears a page only if `page.revision` still equals the snapshot's (`checkpoint.rs:592-596`). It clears a fact key only if the key's newest LSN still equals the snapshot's (`checkpoint.rs:602-608`, `atticdb-core/README.md:122`). Then it sets the watermark (`checkpoint.rs:618`). Retry entries need no dirty mark: a delta takes every entry above the watermark (`checkpoint.rs:398-403`). `engine_sim::publish_during_checkpoint_keeps_the_new_page_dirty` (`engine_sim.rs:1398`) pins the page and fact cases with a crash and reopen.
  - Derived-data readers: `Engine::sessions` (`engine.rs:1339`) adds every published turn to `total_turns` and each session's `turns` (`engine.rs:1354-1355`, `1373-1377`). `Engine::metrics` (`engine.rs:1429`) reports `facts` as `state.facts.map.len()` (`engine.rs:1459`), plus `fact_revisions` (`1460`), `compressed_turns` (`1463`) and `search_index_terms` (`1466`).
  - `Metrics.facts` and `fact_revisions` are at `engine.rs:335` and `337`. Readers snapshot the durable LSN at `engine.rs:916` (window) and `engine.rs:1388` (`fact_snapshot`). Turns past the snapshot are skipped at `engine.rs:952`. `Revs.dropped` is at `facts.rs:20-26`.
  - New: the word index (`search_index.rs`) holds lowercase tokens and LSN postings per tenant, in RAM. `candidates` (`search_index.rs:185-219`) only proposes. The builder decodes, and so inflates, every durable turn (`search_index.rs:293-304`).
  - New: the hot cache (`hot_cache.rs`) holds decoded turns keyed by LSN. It has `get`, `insert` and LRU eviction, and no remove-by-predicate.
- Server and clients (`atticdb/src`):
  - Routes are at `api.rs:71-83` (11 routes). `/v1/records?kind=forget` is a 400 (`api.rs:708`), tested at `atticdb/tests/it/api.rs:269` (was 264).
  - Every response has 1 `Content-Length` (`http.rs:510-525`). The client refuses chunked replies (`http.rs:683-694`). `MAX_RESPONSE_BYTES` (`api.rs:31`, about 49 MiB) bounds client reads, not every server route. A window is at most 8 MiB (`api.rs:25`). Window bodies take the `SENDING` budget (`api.rs:322-326`, used in `send`, `api.rs:558-575`).
  - `/v1/stats` reports `facts` (keys held, tombstones included) and `fact_revisions` (`api.rs:263-319`). New: `GET /metrics` renders the same fields for Prometheus (`api.rs:234-261`). New: `turn_repeats` keeps a 64-bit hash of recent appended contents in 4,096 RAM slots (`api.rs:229-231`, `350-366`).
  - `tools/call` runs any name `Client::call` knows (`mcp.rs:168-178` into `client.rs:286-298`, was `mcp.rs:393-452`). CLI verbs go `main.rs:134-154` to `tool()` (`main.rs:205-208`) to `client::command` (`client.rs:44-90`). Only `fact_set` is marked destructive (`mcp.rs:256`, was 693). There are 8 tools (`mcp.rs:240-249`).
  - Tool text moved to `client.rs`: turns `808-822`, hits `771-795`, fact `744-752`, facts `754-769` (was `mcp.rs:923-974`).
  - New: `file_ref` reads the current ref before it writes (`client.rs:381-420`). `file_get` reads the current ref and, with `versions`, the history (`client.rs:423-495`). Folders list through `fact_list` (`client.rs:498-524`). Keys come from `file_key` and `resolve` (`client.rs:826`, `858`).
  - The hook prints each note's `content` (`hook.rs:146-147`) and orders notes by `commit_time_ns`, then `lsn` (`hook.rs:170-182`). It reads through `HomeReader` (`client.rs:93-125`), and so does setup's store check (`setup.rs:510-511`, result dropped).
  - `atticdb dump` prints payload lengths, never content (`ops.rs:199-220`, was 178). `ops::dump` (`ops.rs:200`) prints kind, tenant id, `bytes=` from `r.payload.len()` and `commit_time_ns` for every record (`ops.rs:211-217`).
  - `ops::stats` (`ops.rs:25`) prints `GET /v1/stats` on the Unix socket. `ops::verify` (`ops.rs:182`) opens a copy and prints the same `stats_fields` (`ops.rs:189`).
- Studio:
  - It forwards 9 read routes (`studio.rs:59-69`, was 57-67). `check` (`studio.rs:318-345`) enforces an exact `Host`, then `Origin` equal to its own when sent, then GET only (`studio.rs:323`), then the secret in `X-Atticdb-Studio` for `/api/` (`studio.rs:348-358`, constant time). So today a POST to any path gets 405, and an authorized GET to an unknown `/api/` path gets 404 (`studio.rs:339-342`). Each connection takes 1 request with no body (`body_bytes: 0`, `studio.rs:293`). The CSP holds `connect-src 'self'` and `form-action 'none'` (`studio.rs:38-48`). The token stays in the bridge (`studio.rs:374-392`).
  - Flags: `start` removes `--no-open` (`studio.rs:99`). `main.rs:133` allowlists studio's flags. USAGE calls studio a read-only page (`main.rs:30`, `main.rs:50`).
  - New: `/api/block` builds the chat-start block with `hook::block_for` (`studio.rs:395-421`).
  - The page prints turn, record and revision content (`studio/app.js:806`, `app.js:327`, `app.js:1025`, current value `app.js:1013`). Its counts come from `/v1/stats` and session totals (`app.js:639`, `app.js:677-686`). New: the sessions screen totals (`app.js:752-753`), the sessions table (`app.js:733`), and the session screen's newest turn (`app.js:876`).
- Tests:
  - CLI rows live in `mcp::tool_calls_over_stdio` (`atticdb/tests/it/mcp.rs:427-461`, was 434-453).
  - `listen::startup_checks` (`atticdb/tests/it/listen.rs:100`) runs `stats`, `verify` and `dump` on a real data dir (`listen.rs:430-463`). It already asserts dump shows no content (`listen.rs:461`).
  - 0003 still cites `api::append_window_and_retry_over_both_listeners` and `api::facts_over_http` (0003:115, 0003:117). Neither exists. The real test is `api::writes_reads_and_retries_over_both_listeners` (`atticdb/tests/it/api.rs:19`).
  - New tests to reuse: `search::search_with_the_index_equals_a_full_scan` (`atticdb-core/tests/it/search.rs:455`), `engine::compression_migrates_one_way_and_retries_match_across_modes` (`atticdb-core/tests/it/engine.rs:97`), `mcp::file_refs_keep_versions` (`atticdb/tests/it/mcp.rs:537`). `engine_sim` rotates `search_walk_first` (`engine_sim.rs:652`), compression (`:663`) and the hot cache on or off (`:993`). Its `Oracle` is at `:125`, `check_fact` at `:176`, `verify` at `:226`, and the `Unknown` settling at `:701-736`.
- Gone: the old review's "no content-returning reader was missed". Main added the hot cache, the index, compressed turns, file refs, client-side renderers and studio's block, all listed below.

## Every read path on main that returns turn, fact or file-ref content

| Path | Where | Hidden by | How |
|---|---|---|---|
| `window`, tape read | `engine.rs:980-1010` | Task 1 | A hidden turn becomes a placeholder. It gets no tape read and no inflate, and costs 0 budget bytes |
| `window`, hot cache hit | `engine.rs:959-979` | Task 1 | The forget check runs before `hot.get`, so a hidden LSN never reaches the cache. `Engine::forget` purges covered entries after the ack, for hygiene |
| Compressed turns | `turn.rs:153` | Task 1 | Search and records parse with `decode_ref`, check, then inflate only visible turns |
| `fact_get`, `fact_list` | `engine.rs:1028-1041`, `1072-1110` | Task 1 | A hidden newest revision means no current value, never an older one (I9) |
| `fact_history` | `engine.rs:1043-1070` | Task 1 | Placeholder `{lsn, redacted: true}`, with no tape read |
| `search`, walk phase | `engine.rs:1204-1209` | Task 1 | `pick` skips hidden turns and facts. No placeholder |
| `search`, word index and trigram lookup | `search_index.rs:185-219`, `engine.rs:1210-1219` | Task 1 | The index returns LSNs only, and the same `pick` checks each one (D96). RAM remanence is documented in Task 6 |
| `records` | `engine.rs:1228-1274` | Task 1 | Placeholder with kind, LSN and identifiers |
| `sessions` and totals | `engine.rs:1339-1383` | Tasks 1 and 3 | Visible metadata. Names stay. Each session's `turns` and `total.turns` keep counting forgotten turns (`engine.rs:1354-1355`). They equal the dense seq range a window returns, placeholders included. Not changed to visible-only counts: an LSN-list forget would make that O(turns) under the state lock. New `forgotten_up_to` and `as_of_lsn` |
| `Engine::metrics` | `engine.rs:1429-1473` | Task 1 | Visible metadata. `facts` keeps forgotten keys (`1459`), `fact_revisions` forgotten revisions (`1460`), `compressed_turns` forgotten turns (`1463`), `search_index_terms` hidden tokens (`1466`). No content. 3 new gauges |
| Retry replies | `engine.rs:620-649` | none | Receipts carry no content. Guess confirmation is documented |
| Checkpoints | `checkpoint.rs` | Task 1 | LSNs, digests and the forget set. No content |
| HTTP JSON | `api.rs:543-727`, `831-853` | Task 3 | Placeholder JSON |
| `/v1/stats`, `/metrics` | `api.rs:234-319` | Task 3 | Visible metadata: the `Engine::metrics` counts above, forgotten records included. 3 new gauges |
| `atticdb stats` | `ops.rs:25` | Task 3 | Visible metadata. Prints `/v1/stats` as is. Unix socket only, all tenants |
| `atticdb verify` | `ops.rs:182` | Task 3 | Visible metadata. The same `stats_fields` on a copy (`ops.rs:189`) |
| `atticdb dump` | `ops.rs:200` | Task 3 | Visible metadata. Every record keeps its line: kind, tenant id, `bytes=` (raw payload length, compressed length for version 2 turns) and `commit_time_ns` (`ops.rs:211-217`). Still never content |
| MCP `memory_recent`, `memory_search`, `fact_get`, `fact_list` | `client.rs:349-374`, `550-579`, renderers `744-822` | Task 3 | Text `#<seq> (forgotten)`. The other tools never see hidden items |
| MCP `file_ref`, `file_get` | `client.rs:381-524` | Task 3 | Versions skip placeholders. `on_disk` matches visible versions only. A forgotten current ref reads as "no ref" |
| CLI `recent`, `search`, `fact get`, `fact list`, `file get` | `main.rs:134-154` | Task 3 | The same structured output as MCP |
| Session-start hook | `hook.rs:118-182` | Task 3 | Placeholders are dropped before `newest`. Facts are already skipped |
| Studio `/api/block` | `studio.rs:395-421` | Tasks 3 and 4 | The hook's code |
| Studio forwarded reads | `studio.rs:59-69`, `374-392` | Task 3 | Server placeholders |
| Studio page | `app.js:327`, `806`, `1013-1025`, counts | Task 4 | "Forgotten" pills, no text |
| Export | new | Task 5 | Placeholders. Forgotten facts are left out |
| `setup`'s store check | `setup.rs:510-511` | none | No content. The result is dropped |
| Python client | `clients/python/atticdb_client.py:186-209` | none | Passes placeholder JSON through |
| `turn_repeats` | `api.rs:350-366` | Task 6 | A count. Documented as a guess channel |

"Visible metadata" means the row is listed in "What a forget leaves visible" and pinned by a test that it doesn't change on a forget. Tasks 1 and 3 add those tests.

## What a forget leaves visible

Every user-facing surface uses this exact sentence: "Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159)." Studio's confirm dialog, `atticdb forget` output and the README point to this list. The README carries it verbatim under the heading "What a forget leaves visible" (Task 6).

A forget hides turn content, meta, role and time, and fact values. These stay observable:
- Names: agents, sessions, fact keys and file-ref paths. `/v1/sessions`, `records` placeholders, studio's tables and list cursors show them.
- Positions: LSNs, seqs and record kinds. Placeholders keep them. Search and list cursors may name a hidden LSN or key.
- Counts: each session's `turns`, session totals, and the `/v1/stats` and `/metrics` fields `facts`, `fact_revisions` and `compressed_turns`. They include forgotten records. `search_index_terms` includes them until the next restart.
- Sizes and times: `atticdb dump` prints every record's payload length and commit time.
- Retry oracles: a retry with a guessed payload and a known retry key confirms the guess. `turn_repeats` counts repeated content.
- RAM: in-memory index tokens stay until the next restart. Hot cache entries stay until purged or evicted. Reads never serve either.
- Bytes: segment files, unsalted retry digests in checkpoints, filesystem and SSD remanence, copies, backups and exports already taken.
- `recover --truncate-at` below a forget's LSN drops the forget, and the content shows again.
- A retry that arrives after its key left the dedup window writes the content again.

**Rules:**
- No new crates. No new features on existing pins. No `unsafe`.
- Every write is durable before the ack. Forget uses the same `Durability::Durable` append as facts.
- Tests follow `TESTING.md`: 1 scenario per path, extend existing tests first, table rows over copies. Sims carry I10.
- Docs: plain short sentences, no em dashes.
- Commits: `git commit -s`.

---

## Task 1: the engine writes, keeps and hides forgets (1 PR)

**Files.** Modify `atticdb-core/src/{engine.rs, machine.rs, dedup.rs, checkpoint.rs, state.rs, options.rs, hot_cache.rs, forget.rs, lib.rs}`, `atticdb-core/README.md:119-125` and `atticdb-core/tests/engine_sim.rs` (1 new test, not the oracle). Create `atticdb-core/src/forgets.rs`. The README gets the tags and 1 line next to `:122`: install clears a forget entry's dirty mark only if its cutoff, or for an LSN pair its presence, is what the snapshot held.

**Write path.**
- `Engine::forget(tenant, Selector, idem_key, up_to_lsn) -> Result<Forgotten>`. `Forgotten` is `{txn, lsn, cutoff, deduplicated}`. It uses `start` (`engine.rs:620-649`) as `write_fact` does. Add `enqueue_forget_sim` next to `enqueue_fact_sim` (`engine.rs:726-741`).
- Prepare (`machine.rs:91`): a forget batch is 1 `Forget` record plus an optional `TenantCreate`. Mixed batches stay `InvalidInput`. An empty LSN list, LSN 0 and `up_to_lsn = 0` are `InvalidInput`. A duplicate LSN counts once.
- Retry scope: new `dedup::Scope::Forget` (`dedup.rs:12-15`), keyed by (tenant, `idem_key`).
- `cutoff = min(up_to_lsn, own LSN)`. Replay computes it again from the payload and the record's LSN.
- Install replaces the `_` arm at `machine.rs:301` for `Kind::Forget`. Replay rebuilds the retry entry as facts do (`machine.rs:296-299`).
- Conditional writes see forgets: at `machine.rs:162-169`, a hidden newest revision counts as LSN 0. Forgets staged in the same group count too, through a staged overlay like `self.facts`.
- Update the doc at `forget.rs:22`.

**The forget set**, in `forgets.rs`, held in `State`:

| Selector | Stored | Bound (I12) |
|---|---|---|
| Session | `HashMap<Key, u64>`: the highest cutoff | Only sessions with published turns, so at most `max_sessions` |
| Fact key | `HashMap<FactKey, u64>`: the highest cutoff | Only keys in the fact index, so at most `max_facts` |
| LSN list | `HashSet<(u32, u64)>`, sorted only while a checkpoint is written | New `max_forgotten_lsns`, default 100,000 |

- No-op detection covers session and fact selectors only. A session selector whose session has no published turn in the request tenant stores nothing. A fact selector whose key isn't in the request tenant's fact index stores nothing. A cutoff at or below the stored one changes nothing and marks nothing dirty.
- LSN selectors store every deduplicated `(request_tenant, lsn)` with `lsn <= cutoff`. LSNs above the cutoff are dropped. Install doesn't check that a pair names a real record of that tenant. A foreign or nonexistent pair stays inert: `hides_turn` and `hides_fact` look up the record header's tenant id with the record's LSN, so the pair never matches. Inert pairs count toward the cap.
- Over the LSN cap, prepare rejects with `ResourceExhausted("forgets")`, counting staged LSNs as `Facts::full` does (`facts.rs:53-57`). Prepare counts the deduplicated list, since it can't know the cutoff's effect. Prepare does no tape reads.
- I11: every entry carries the record header's tenant id.
- Helpers `hides_turn(&Key, lsn)` and `hides_fact(&FactKey, lsn)`. Both are O(1).

**Readers.**
- `window_snapshot`: read the session cutoff under the first lock (`engine.rs:913-931`). Check the LSN set under 1 short lock per page. A hidden turn becomes a placeholder before `self.hot.get` (`engine.rs:959`): `redacted: true`, seq and LSN, empty content and meta, `commit_time_ns` 0, role `System`. Count `window_reads` only for visible turns. A window that started before the ack linearizes before it.
- Hot cache: add `HotCache::purge(|lsn, cached| ...)`. `Engine::forget` calls it after the ack, outside the state lock. Correctness never depends on it.
- `fact_get`, `fact_list`: a hidden newest revision means none. `fact_history`: placeholder revisions, with no `read_facts` call (`engine.rs:1393-1421`).
- `search`: in `pick`, turns go through `decode_ref` and the hide check before inflate and match. `current` (`engine.rs:1125-1130`) adds `hides_fact`. The walk and the index candidates share `pick`. Hidden records still count as visits.
- `records`: placeholders with kind, LSN, and agent, session and seq, or the key. Use `decode_ref`. The size counts identifiers. Remove the comment at `engine.rs:1229`.
- `sessions`: add `SessionInfo.forgotten_up_to: Option<u64>`, the session cutoff. Totals are unchanged: they stay visible metadata (see the inventory). Add `SessionPage.as_of_lsn: u64`: `self.tape.durable_lsn()` read right after the lock (`engine.rs:1343`), as `window_snapshot` does. Not `state.published_lsn`: the state machine publishes before the tape stores `durable_lsn` (`atticdb-tape/src/writer.rs:776-784`), so publication can briefly run ahead. The count may include a turn above `as_of_lsn`. A forget pinned to it then hides less, never more.
- `window_snapshot`: add `Window.as_of_lsn: u64`, set to the existing `snapshot` (`engine.rs:916`), the same value that filters turns at `engine.rs:952`. So every turn the window shows is at or below it, and no turn above it shows. Never `state.published_lsn`: it can be ahead of `snapshot` while a group publishes, and a forget pinned to it would hide a turn the window skipped. Studio pins a forget to `as_of_lsn` (Q8).
- Add `pub redacted: bool` to `Turn` and `Revision`.
- Metrics: `forgotten_sessions`, `forgotten_fact_keys`, `forgotten_lsns`. The existing counts stay as they are (`engine.rs:1459-1466`): visible metadata.

**Checkpoints (I8).** New tags: 6 forget retry (tag 5 layout), 7 session cutoff, 8 fact-key cutoff, 9 up to 1,024 LSNs of 1 tenant. Load checks tenants and `lsn <= boundary`, as tag 4 does (`checkpoint.rs:270-279`). Cutoffs and stored LSNs are at most their forget's own LSN, so the check holds. A pre-S5 binary fails replay at `machine.rs:301` with `corrupt_index`, with no truncation (Q9).

**Dirty tracking (race-safe), mirroring fact keys.** A forget published after the snapshot (`checkpoint.rs:488`) but before the dirty clear must stay dirty. Otherwise the next checkpoint's boundary passes its LSN without writing it, and a restart from that checkpoint skips its replay and loses it.
- `Forgets` holds 3 dirty sets: `dirty_sessions: BTreeSet<Key>`, `dirty_keys: BTreeSet<FactKey>` and `dirty_lsns: BTreeSet<(u32, u64)>`. Install, and so replay, marks an entry dirty whenever it inserts it or raises its cutoff. A no-op marks nothing.
- `snapshot` (`checkpoint.rs:342`) copies values, not references. Full: every entry. Delta: only dirty ones. Sessions and keys go in as `(key, cutoff)`. LSNs go in as pairs.
- After the manifest rename, `save` clears in chunks under the state lock, as at `checkpoint.rs:602-608`:
  - a session or fact key only if its current cutoff equals the snapshot's cutoff. Cutoffs only grow, so equal means no forget raised it after the snapshot;
  - an LSN pair only if it is in the snapshot. A pair first added after the snapshot isn't in it, so it stays dirty. Re-adding a held pair is a no-op;
  - an entry absent from the snapshot is never cleared.
- Tag 6 needs no dirty set. Forget retry entries live in `state.dedup.entries`, and a delta takes every entry above the watermark (`checkpoint.rs:398-403`).
- `save` writes tags 7 to 9 after the tag 4 loop (`checkpoint.rs:546-558`), before `buffer.flush()` (`checkpoint.rs:559`). The `Scope` match at `checkpoint.rs:529-532` gains `Scope::Forget => (6, ...)`.

**Tests to extend:**
- `machine.rs:442` `forget_and_mixed_fact_batches_reserve_nothing` becomes a table: a lone forget prepares, and a forget next to a turn or a fact is rejected.
- `engine::retries_conflicts_scope_isolation_and_replay`: a forget retry returns the original receipt with `deduplicated`, a changed payload gives `IdempotencyConflict`, reopen works. A retried forgotten append returns its receipt, and the window still shows placeholders.
- `engine::invalid_and_over_limit_writes_are_typed_and_write_nothing`: an empty list, LSN 0, `up_to_lsn = 0`, the LSN cap, a duplicate LSN counted once, and the cap across staged batches.
- `engine::checkpoint_bounds_replay_and_corrupt_derived_data_falls_back`: forget metrics match a full replay after a checkpointed reopen, with no checkpoint, and with a corrupted checkpoint that holds tags 6 to 9.
- `engine::windows_are_dense_and_read_cost_depends_only_on_returned_turns`: read a window (caching it), forget the session and then LSNs, read again. Placeholders are dense, `window_reads` and `hot_cache_hits` grow only by visible turns, `hot_cache_bytes` drops after the purge, and a later turn shows.
- `engine::compression_migrates_one_way_and_retries_match_across_modes`: a forgotten compressed turn is a placeholder in `window`, absent from `search`, and a placeholder with identifiers in `records`.
- `facts::put_supersede_delete_condition_and_reopen`: forgetting the current value gives None, `list` skips the key, and history shows a placeholder. A later put is current. `up_to_lsn` below the current value hides only older revisions. `if_lsn = 0` after a forget succeeds and the old LSN gets `Conflict`. A forget aimed at a Forget record's LSN undoes nothing. `up_to_lsn` above its own LSN is clamped. Reopen keeps all of it.
- `search::search_finds_current_records_newest_first` and `search::sessions_and_records_list_what_a_tenant_wrote`: hidden turn and fact rows. Tenant `other` forgetting `tenant`'s LSNs changes nothing, yet `forgotten_lsns` counts those pairs. An LSN that names no record is stored and inert. With `up_to_lsn` below some listed LSNs, only those at or below it are stored. A session selector for a session with no turn, or a key that was never set, leaves `forgotten_sessions` and `forgotten_fact_keys` at 0. `forgotten_up_to` is set. `as_of_lsn` equals the durable LSN after the last write. Visible metadata rows: after forgetting a session, a fact key and LSNs, the `sessions` helper line (`search.rs:84-98`) shows the same `total` and per-session `turns` as before the forgets.
- `facts::put_supersede_delete_condition_and_reopen`, visible metadata row: `metrics().facts`, `fact_revisions` and `compressed_turns` are the same before and after forgetting a key, and after reopen. `forgotten_fact_keys` is 1.
- `search::search_with_the_index_equals_a_full_scan`: forgets in the corpus. Pages with the index equal a full walk, and no page holds hidden content.
- New `engine_sim::publish_during_checkpoint_keeps_new_forget_dirty`, next to `publish_during_checkpoint_keeps_the_new_page_dirty` (`engine_sim.rs:1398`), with the same `after_nth(&io, 2, ...)` hook:
  - Seed turns in sessions A and B, facts `k` and `j`, then checkpoint. Forget session A up to its 2nd turn, key `k` up to its 1st revision, and LSN `x`. Append 2 more turns to A and put `k` again.
  - Queue with `enqueue_forget_sim`: A with a higher cutoff (raised entry), key `j` (new entry), `k` with a higher cutoff (raised entry), and LSNs `x` again (no-op) and `y` (new pair).
  - The hook steps them during `engine.checkpoint()`, after the snapshot and before the clear. That checkpoint's boundary is below the queued forgets. Finish the queued replies, then checkpoint again: its boundary is above them.
  - Crash with `io.inner.crash()` and reopen. `replayed_records` is 0, so nothing came from replay. Windows, `fact_get` and `records` hide exactly what the queued forgets hide. `forgotten_*` metrics equal their values before the crash.
  - Reopen once more with the checkpoint files removed. A full replay gives the same answers.
- `checkpoint::deltas_compaction_cold_reload_and_corruption_fallback` (`checkpoint.rs:947`): a delta after a forget writes only the dirty entries. A delta with no new forget writes no tag 7 to 9 record. A full checkpoint writes them all.

**Acceptance:** `cargo test -p atticdb-core --test it`, `cargo test -p atticdb-core --release --features sim --test engine_sim publish_during_checkpoint`, clippy and fmt pass. `Forgets are not applied yet` is gone. `bench/reads.py --target-mib 512`, interleaved before and after with an empty forget set: window, fact and search p50 are within 5% of main.

## Task 2: the I10 sim oracle

In `atticdb-core/tests/engine_sim.rs`:
- The workload adds forgets in about 1 batch in 10: a scope's session, a `FACT_KEYS` key, or 1 to 8 acked LSNs, sometimes another tenant's, sometimes with `up_to_lsn`.
- `Oracle` (`:125`) keeps acked forgets per tenant. An `Unknown` forget settles by retry (`:701-736`).
- `verify` (`:226`) and `check_fact` (`:176`) expect placeholders and I9. Windows are read before and after each forget, so the hot cache (on in 2 of 3 seeds, `:993`) holds turns that become hidden. A full `records` walk and a `search` per written word find no hidden content. `search_walk_first` (`:652`) and compression (`:663`) rotations cover the index and inflate paths.
- The check goes both ways: hidden exactly when a forget of the same tenant covers the record.
- Reopens from a checkpoint and after damage give the same answers. No change to the tape sim.

**Acceptance:** `ATTICDB_SIM_SEEDS=2000 cargo test -p atticdb-core --release --features sim --test engine_sim` passes.

## Task 3: `POST /v1/forget`, the CLI verb, and the server's and clients' readers

**Files.** Modify `atticdb/src/{api.rs, serve.rs, client.rs, main.rs, mcp.rs, hook.rs}`. Tests: `atticdb/tests/it/{api.rs, mcp.rs, hook.rs, listen.rs}`.

**HTTP.**
```
POST /v1/forget
{"idem_key": "k", "session": {"agent": "a", "session": "s"}}   or "key": "k"   or "lsns": [12, 15]
optional "up_to_lsn": n
200 {"outcome": "committed", "deduplicated": false, "txn": 9, "lsn": 40, "cutoff": 40}
```
Exactly 1 selector, else 400. Add it to `Route` and `ROUTES` (`api.rs:42-83`, 12 routes) and to `serve.rs:173-183`. Errors go through `write_response` (`api.rs:498`).

**CLI.**
```
atticdb forget session <connect> --agent <a> --session <s> [--up-to-lsn <n>]
atticdb forget key <connect> --key <key> [--up-to-lsn <n>]
atticdb forget file <connect> --path <file> [--up-to-lsn <n>]
atticdb forget lsns <connect> --lsns <n,n,...> [--up-to-lsn <n>]
```
- `forget session` needs `--agent` and `--session` given explicitly, with no default to `mcp` and the repo.
- `forget file` derives the key with `resolve` and `file_key` (`client.rs:826`, `858`), so a deleted file's ref can still be forgotten.
- New `client::forget_command` uses `Client::write` (`client.rs:527-548`). It is not in `Client::call` (`client.rs:286-298`), so `tools/call memory_forget` stays -32602 (`mcp.rs:175`).
- Add `forget` to `HOME_COMMANDS` (`main.rs:57-58`), `USAGE` (`main.rs:12-53`) and the dispatch (`main.rs:134-154`).
- Output on success, on stdout, exactly 3 lines:
  ```
  forgot session a/s up to lsn 40 (txn 9, lsn 40)
  Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159).
  Names, keys, LSNs, counts and sizes stay visible. See "What a forget leaves visible" in the README.
  ```
  The first line names the selector (`session a/s`, `key k`, `file <path>`, `3 lsns`), the cutoff, and txn and LSN. A deduplicated retry adds ` (already done)` to it. `USAGE` for `forget` carries the 2nd line too.

**MCP: no `memory_forget` tool (Q1).** The CLI is friction, not a guaranteed approval (D88). Add 1 sentence to `INSTRUCTIONS` (`mcp.rs:225`): to forget, the user runs `atticdb forget` or uses studio.

**Readers.**
- `turn_json` and `revision_json` (`api.rs:831-853`): `{seq, lsn, redacted: true}`, or `{lsn, redacted: true}`. `record_json` (`api.rs:727-736`) adds kind and identifiers.
- Sessions JSON (`api.rs:681-690`) adds `forgotten_up_to` per session and a top-level `as_of_lsn`. `turns` and `total` are unchanged: visible metadata.
- Window JSON (`api.rs:550-553`) adds `as_of_lsn` next to `truncated`. Studio pins its forgets to it (Q8).
- `stats_fields` (`api.rs:263-319`) adds the 3 gauges, which `/metrics`, `atticdb stats` (`ops.rs:25`) and `atticdb verify` (`ops.rs:182`) then show. The other fields keep counting forgotten records. `atticdb dump` (`ops.rs:200`) is unchanged.
- `client.rs` `turns` (`808-822`) prints `#<seq> (forgotten)`. `file_get` versions (`452-488`) skip placeholders, and `on_disk` matches visible versions only.
- Hook: drop redacted turns before `newest` (`hook.rs:146`, `170-182`).

**Tests:**
- `api::writes_reads_and_retries_over_both_listeners`: each selector over both listeners. Placeholders in window, history and records. A deduplicated retry, a null fact, a later write that shows, the gauges in `/v1/stats` and `/metrics`, and `forgotten_up_to`. `as_of_lsn` in sessions and window JSON equals the last acked LSN. Visible metadata rows: sessions `turns` and `total`, and `/v1/stats` `facts`, `fact_revisions` and `compressed_turns`, are the same before and after the forgets.
- `api::auth_and_host_rules`: tenant B's forgets leave A unchanged. 400 rows for 2 selectors, none, an empty list, LSN 0, `up_to_lsn` 0 and a missing `idem_key`. `GET /v1/forget` gets 405.
- `mcp::tool_calls_over_stdio`: CLI rows for the 4 verbs, each printing the 3 lines above, `(forgotten)` in `recent`, still 8 tools, `tools/call memory_forget` gets -32602, and `forget session` without `--session` exits 2.
- `listen::startup_checks` (`listen.rs:100`): forget the turn `two` with `atticdb forget lsns` right after `last` is computed (`listen.rs:427`), so `last` grows by 1 for the forget record. Then `dump` (`listen.rs:456-463`) still prints that LSN with the same `bytes=` as before, and no content. `stats` and `verify` (`listen.rs:430`, `449`) print `forgotten_lsns=1`, and `facts` and `fact_revisions` as before.
- `mcp::file_refs_keep_versions`: after `forget file`, there's no ref, history shows no versions, the folder skips it, and a new save makes a fresh ref.
- `hook::session_start_prints_the_memory_block`: a forgotten note and a forgotten rule never show.

**Acceptance:** `cargo test -p atticdb --test it api:: mcp:: hook:: listen::` passes, plus the Windows clippy line in TESTING.md.

## Task 4: studio forgets behind `--allow-forget` (amends D87 with D99)

**Files.** Modify `atticdb/src/{studio.rs, main.rs}`, `atticdb/studio/{app.js, app.css}`, `atticdb/tests/it/{studio.rs, support.rs}`, `atticdb-core/tests/engine_sim.rs` (1 race test), `atticdb/README.md:19`, `:632`, and `docs/design/0002-decision-log.md` (D99).

**Read-only by default (Q7).**
- New flag `--allow-forget`. `start` removes it as it removes `--no-open` (`studio.rs:99`) and passes a `forget: bool` into `Studio`. `main.rs:133` adds it to studio's allowed flags. `USAGE` (`main.rs:30`, `:50`) says: "studio serves a read-only page on 127.0.0.1 and opens it. With --allow-forget it can also forget sessions and fact keys."
- Without the flag, studio is today's binary. `check` (`studio.rs:318-345`), `READS` and `body_bytes: 0` (`studio.rs:293`) stay as they are. `/api/forget` isn't a route, so it gets what any unregistered path gets today: an authorized GET is 404 (`studio.rs:339-342`), and a POST is 405 from the method check (`studio.rs:323`).
- The flag registers the forget route and nothing else. It covers session and fact-key forgets. Studio has no fact edit route in S5. Any later fact edit route goes behind this same flag.
- stdout gets 1 more line at start with the flag: "atticdb studio: forget is on (--allow-forget)."

**Bridge, with `--allow-forget` only.**
- 1 write route: `POST /api/forget`. `check` order: `Host`, `Origin`, method, `Sec-Fetch-Site`, secret, `Content-Type`.
  - Writes need exactly 1 `Origin` equal to the studio's own: 403 otherwise.
  - POST only on this path. Every other non-GET stays 405. OPTIONS gets 405, and no `Access-Control-*` header is ever sent.
  - `Sec-Fetch-Site`, when sent, must be `same-origin`: 403 otherwise.
  - The secret is required (401).
  - `Content-Type` must be exactly `application/json` (415 `unsupported_media_type`). That forces a CORS preflight, which never gets an answer.
  - `Limits.body_bytes` becomes 64 KiB (413). GET bodies stay refused by `http.rs`.
- Body: `{"confirm": "hide", "forgets": [{"idem_key": "<32 hex>", "session": {"agent": "a", "session": "s"}, "up_to_lsn": 812}]}` for 1 to 100 sessions, or 1 item with `"key"`. Every item needs `up_to_lsn`, a positive integer (Q8). Anything else is 400.
- The bridge sends each item in order as `POST /v1/forget` with its token, through a new `post` helper beside `get` (`studio.rs:141`). It stops at the first failure and marks the rest `not_sent`. Reply: `{"results": [...]}`.
- stderr gets 1 line per request, with counts only and no names.
- `/api/info` adds `"forget": true|false`, true only with `--allow-forget`.

**Page.**
- With `/api/info` `"forget": false` the page draws no checkbox, no forget button and no dialog. Pills and count labels still show, since placeholders come from the server either way.
- Sessions table (`app.js:733`): a checkbox column, a header checkbox that selects every shown row, and a "Forget selected (N)" button. Rows with `forgotten_up_to` get a "Forgotten" pill.
- Session screen: a "Forget this session" button. Placeholder turns show `Turn N` and a "Forgotten" pill with no body. The finding at `app.js:876` uses the newest visible turn.
- Fact screen: a "Forget this key" button. Placeholder revisions get a pill. A forgotten newest revision reads "No current value. This key was forgotten."
- Log rows (`app.js:322-338`): placeholders get a pill. Counts (`app.js:639`, `677-686`, `752-753`) say forgotten turns are included.
- Pinning (Q8): the sessions table sends its page's `as_of_lsn` from `/v1/sessions` as every item's `up_to_lsn`. The session screen sends its window's `as_of_lsn`. The fact screen sends the LSN of the newest revision it shows. So a turn or revision written after the view loaded stays visible.
- After the reply, the page re-fetches the view it is on (the sessions page, the window or the fact) and redraws it. The selection clears. Items marked `unknown` or `not_sent` stay selected for a retry.
- Confirm step: a `<dialog>` built with `h()` and opened with `showModal()`. It names up to 10 items, then "and N more". It says, in this order:
  - "Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159)."
  - "Names, keys, LSNs, counts and sizes stay visible." followed by a link, "What a forget leaves visible", to that README section on GitHub, opened with `rel="noopener noreferrer"`.
  - "Turns written after this page loaded stay visible."
  - "AtticDB can't undo this."
  Cancel has the default focus, and Escape cancels.
- Retry keys come from `crypto.getRandomValues`. A retry after `unknown` or `not_sent` resends the same keys.
- Store text stays text-only. No `innerHTML`, no inline handlers. The CSP is unchanged.

**Tests:** extend `studio::studio_forwards_reads_and_refuses_the_rest`.
- A default studio (no flag): `/api/info` has `"forget": false`. GET `/api/forget` gets 404 and POST `/api/forget` gets 405, the same rows as any unregistered path today. Then the rows below run on a second studio started with `--allow-forget`.
- Rows for POST `/api/forget`: no Origin 403, a foreign Origin 403, `Sec-Fetch-Site: cross-site` 403, no secret 401, `text/plain` 415, form-encoded 415, no confirm 400, a session item without `up_to_lsn` 400, `up_to_lsn` 0 400, 101 items 400, mixed selectors 400, a body over 64 KiB 413, OPTIONS 405 with no `Access-Control-Allow-Origin`, and GET `/api/forget` 405. The existing `POST /api/v1/sessions` row stays 405.
- Then load `/api/v1/sessions` and keep its `as_of_lsn` L. Append 1 turn to session X through the store, after L. Bulk forget X and Y with `up_to_lsn` L. Its retry with the same keys comes back `deduplicated`. Re-fetch, as the page does: X's turns at or below L are placeholders, the turn after L shows, and X's `forgotten_up_to` is L. Forwarded records hold placeholders, and `/api/block` drops the note.
- A data-dir studio started with `--allow-forget` forgets too.
- The publish race, in `engine_sim.rs`, since only the sim can hold a group between publish and the durable watermark. New `window_as_of_lsn_never_covers_an_unseen_turn`, built like `inline_group_publishes_once_and_replies_with_original_sequences` (`engine_sim.rs:1233`) with `PublishPause` (`atticdb-tape/src/sim.rs:24`):
  - Append 2 turns to session X. Arm the pause, enqueue a 3rd turn with `enqueue_sim`, and run `step_sim` on a thread until it parks after publish, before `durable_lsn` advances.
  - While parked, read the window and the sessions page. Neither `as_of_lsn` reaches the 3rd turn's LSN. Both equal `metrics().durable_lsn`. The window shows seqs 1 and 2 only.
  - Release the pause and finish the reply. Forget X with `up_to_lsn` set to the window's `as_of_lsn`, as the page would.
  - The window now shows seqs 1 and 2 as placeholders and seq 3 with its content.

**Acceptance:** `cargo test -p atticdb --test it studio::` and `cargo test -p atticdb-core --release --features sim --test engine_sim window_as_of_lsn` pass. Manual check without the flag: no checkbox, no button. With `--allow-forget`: select, cancel, confirm, see pills and the refreshed view, reload. The D99 row is in 0002.

## Task 5: markdown export

**Files.** Modify `atticdb-core/src/engine.rs`, `atticdb/src/{api.rs, serve.rs, main.rs, lib.rs}`. Create `atticdb/src/export.rs` and `atticdb/tests/it/export.rs`.

**Engine.** `Engine::export(tenant, ExportQuery {at, after, max_bytes, max_items}) -> ExportPage`.
- Order: facts first, file refs included under their `file/` keys, keys in byte order. Then sessions in byte order of agent, then session, each by seq.
- `after: Option<ExportPos>`. `ExportPos` is `Fact { key }` or `Turn { agent, session, seq }`. The page starts at that entry, inclusive.
- `ExportPage` is `{at, entries, done}`. Each entry is a `StoredFact` or a `Turn`, placeholders included. Forgotten facts are left out.
- It stops after `max_items` entries, or once raw content and meta bytes pass `max_bytes`. The first entry always comes. Raw bytes never exceed encoded bytes, so the engine always returns enough for 1 page.
- The first page pins `at` to the durable LSN. Window and fact reads take `at` in place of `engine.rs:916` and `1388`. Turns past `at` are skipped (`engine.rs:952`).
- Export reads use the hot cache but never insert into it, so a full export doesn't evict agents' recent turns.
- A key whose revision at `at` was trimmed (`facts.rs:20-26`) gives `Conflict`. The forget set is read at the current state (D45).

**Rendering** (`export.rs`). Deterministic: the same entry, `at` and forget state give the same bytes.
- Paths are `facts.md` and `sessions/<agent>/<session>.md`, with names encoded as below. No visible facts means no `facts.md`.
- `facts.md` starts `# Facts, LSN <at>`. Per key: `## ` and the key as a JSON string, so a newline can't start a heading. Then a line with LSN, agent and UTC time, then the content in a fence longer than any backtick run in it. Meta follows as 1 JSON line.
- A session file starts `# <agent> / <session>`, both as JSON strings. Per turn: `## #<seq> <role>, <time>, lsn <n>` and fenced content. A placeholder is `## #<seq> forgotten`.
- Content that isn't UTF-8 is rendered lossy, as `blobs` does (`api.rs:811-829`), and its heading ends with ` (lossy)`.
- An entry's markdown is its heading and body. The first entry of a file also carries the file heading.

**Pages cut inside files.** 1 session or `facts.md` can be any size. So a page carries chunks, not whole files, and every page fits `max_bytes`.
- Response: `{"format": "markdown", "at": 812, "chunks": [{"path": "...", "offset": 0, "markdown": "..."}], "next": {...} or null}`.
- `offset` is the chunk's byte offset in its file, in UTF-8 bytes of the markdown.
- `export.rs` renders entries in order and appends them to the current chunk. When the next entry's file differs, it closes the chunk and opens one for the new path at offset 0.
- When the next bytes would pass the budget, it cuts the entry's markdown at the last UTF-8 char boundary that fits and stops. The rest of that entry comes on the next page.
- Every page carries at least 1 char of markdown.
- Entries the engine returned past the cut are dropped and read again on the next page. That costs at most 1 page of extra reads.

**Cursor.** `next` names the entry to resume at:
```
{"at": 812, "key": "k", "skip": 0, "offset": 51200}
{"at": 812, "agent": "copilot", "session": "atticdb", "seq": 41, "skip": 3000, "offset": 1048000}
```
- `skip` is how many bytes of that entry's markdown were already sent. `offset` is the byte count of its file sent so far.
- The client passes it back as query parameters: `at`, then `after_key`, or `after_agent`, `after_session` and `after_seq`, then `skip` and `offset`. Any other mix is 400. A 1,024-byte key percent-encodes to at most 3 KiB, under the 16 KiB head limit (`http.rs:85`).
- The server echoes `offset` into the next chunk. It holds no export state between pages.
- The server re-renders the cursor's entry and checks `skip` before any slice, in this order:
  - `skip > 0` and the entry is now hidden: 409. That happens when a forget lands mid-entry.
  - `skip > markdown.len()`: 409.
  - `!markdown.is_char_boundary(skip)`: 400 `invalid_input`, "skip is not on a character boundary". The server's own cursors always cut at a boundary, so only a malformed cursor gets here.
  - Only then does it slice `&markdown[skip..]`. No cursor value can make it panic.
- A forget that lands between entries just applies to the later pages.

**Encoded-byte accounting.**
- `max_bytes` bounds the whole response body, exactly. Default 1 MiB (`api.rs:23`), at most 8 MiB (`api.rs:25`), at least 64 KiB. Outside that it's 400.
- The budget starts at `max_bytes` minus 8 KiB. That reserve covers the envelope and the worst `next`: a 1,024-byte key at 6 bytes per byte is 6 KiB.
- Each chunk costs its exact `{"path":"…","offset":n,"markdown":""}` overhead, plus the separator comma.
- Markdown costs what serde_json writes: `"` and `\` cost 2, `\n`, `\r`, `\t`, `\b` and `\f` cost 2, other bytes below 0x20 cost 6, and every other char its UTF-8 length.
- After `serde_json::to_vec`, a `debug_assert` checks the body length is at most `max_bytes`.
- `max_items` caps entries per page: default and max 1,000 (`MAX_WINDOW_TURNS`, `api.rs:21`).
- The body takes the `SENDING` budget, as windows do (`api.rs:322-326`, `558-575`).

**HTTP.** `GET /v1/export?format=markdown[&at=&after_*=&skip=&offset=&max_bytes=]`. Add it to `Route` and `ROUTES` and to `serve.rs:173-183`.
- `format` other than `markdown` is 400. `Conflict` is 409 `conflict`: "the export fell behind; start again".

**Name encoding.** Inlined from the 2026-10-30 plan, with its gaps closed.
- Bytes `a-z`, `0-9`, `-` and `_` stay. `.` stays unless it is the first or last byte. Every other byte becomes `%xx` in lowercase hex. That includes `%`, `~`, `/`, `\`, uppercase and non-ASCII bytes.
- So `App` and `app` become `%41pp` and `app`, and can't collide on a case-insensitive disk.
- `.`, `..` and `../x` become `%2e`, `%2e%2e` and `%2e%2e%2fx`. No name holds `/` or `\`, and no name is `.` or `..`. A trailing `.`, which Windows drops, is escaped too.
- `%` is escaped, so decoding is unique and 2 distinct short names never collide.
- An empty name becomes `%`. Every escape is 3 bytes, so a bare `%` is never produced otherwise.
- A name whose part before the first `.` is `con`, `prn`, `aux`, `nul`, `com1` to `com9` or `lpt1` to `lpt9` gets its first byte escaped. Windows reserves those.
- Past 200 encoded bytes: keep the first 180 encoded bytes, cut back so no `%xx` is split, then add `~` and 16 lowercase hex chars of the raw name's BLAKE3. `~` is always escaped otherwise, so a long name never equals a short one. 2 long names collide only if their first 180 bytes and a 64-bit hash match. The client's offset-0 rule catches that and fails.
- Client grammar for each name segment: 1 to 200 bytes of `a-z0-9-_.%~`. It doesn't start or end with `.`. Each `%` is followed by 2 lowercase hex digits, or is the whole segment. A `~` may appear once, followed by exactly 16 lowercase hex digits and nothing else. A path is `facts.md` or `sessions/<segment>/<segment>.md`. Anything else is rejected: an absolute path, `\`, an empty segment, or another depth.

**CLI.** `atticdb export <dir> [<store flags>] [--max-bytes <n>]`. It opens the store as studio does (`studio.rs:106`). It is not an MCP tool and has no studio button.

**Crash-safe assembly.** `<dir>` either doesn't exist or holds a complete export.
- It fails if `<dir>` exists. It creates `<dir>.partial-<16 hex>` beside it, mode 0700.
- It checks each chunk's path against the grammar before any write. A bad path fails the export (exit 1, no restart).
- `offset` 0: create `<path>.part` with `create_new`, mode 0600, and parent dirs 0700. A second offset-0 chunk for a path seen before fails the export: that is a name collision or a server bug.
- `offset` > 0: the path must equal the previous chunk's path, and `offset` must equal the bytes written to that file so far. Otherwise the export fails. Then it appends.
- When a chunk for a new path arrives, or the last page ends, it fsyncs the finished `.part` file and renames it to `<path>`. Final names end in `.md`, so a `.part` name never collides with one.
- After the last page it fsyncs every dir in the tree. It checks `<dir>` is still absent, renames `<dir>.partial-<hex>` to `<dir>`, and fsyncs the parent.
- A crash leaves only `<dir>.partial-<hex>`. The next run doesn't touch it. Any failure prints its path on stderr.
- A 409 deletes the partial folder and starts again from page 1, at most 3 times. Then it exits 1.

**Tests:**
- New `atticdb/tests/it/export.rs`, `export_writes_one_file_per_session_and_facts_at_one_lsn`. Seeds: names that are empty, `App` and `app`, `../x`, `con`, `a.` and 300 bytes; keys full of `"`, newlines and control bytes; a 1 MiB turn; a turn of 256 bytes or more (compressed); a non-UTF-8 turn; a file ref; a forgotten turn and fact; and a write after page 1. With `--max-bytes 65536`, checks:
  - every body is at most 65,536 bytes, and the 1 MiB turn spans pages with continuous offsets;
  - each file equals its parts joined, with placeholders and no forgotten content;
  - nothing is past `at`; file modes are 0600 and 0700;
  - an existing `<dir>` is refused.
- Same file, `export_restarts_when_a_forget_cuts_an_entry`: fetch 1 page that ends inside the 1 MiB turn, forget that turn, and fetch with the cursor: 409. The CLI then restarts and finishes, and the turn is a placeholder.
- Unit tests in `export.rs`:
  - the encoder table above, with every output passing the grammar and no 2 inputs sharing an output;
  - the grammar rejects `../x`, `/abs`, `a\b`, `.hidden`, `x.`, `%4`, `%41%`, `a~1` and 4 segments;
  - the assembler, fed pages from a vector: a repeated offset 0 fails, an offset gap fails, a path switch mid-file fails;
  - an error after page 2 leaves no `<dir>`, and a partial dir that holds only complete `.md` files and 1 `.part` file;
  - the size calculator equals `serde_json::to_vec(...).len()` for random pages.
- A `facts::` row for `Conflict`, and `api::` 400 rows for a bad `format`, `max_bytes` below 64 KiB, and `after_key` mixed with `after_agent`.
- Malformed-cursor rows in `export_writes_one_file_per_session_and_facts_at_one_lsn`: seed a turn of 100,000 `é` chars, which spans pages at 64 KiB. Take a real cursor inside it and add 1 to `skip`, so it lands mid-char: 400 `invalid_input`. `skip` past the entry's end: 409. Non-numeric `skip`: 400. After each, the same server still answers the real cursor with 200.

**Acceptance:** `cargo test -p atticdb --test it export::` and `cargo test -p atticdb --lib export` pass, plus clippy and fmt.

## Task 6: docs and wrap-up (controller)

- README "Forget and export": it opens with "Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159)." Then a subsection "What a forget leaves visible", copied verbatim from that section of this plan. Studio's dialog link and `atticdb forget` output point at it, so the anchor must be `#what-a-forget-leaves-visible`.
- `atticdb forget` is friction, not approval. `dump`, `stats` and `verify` print no content, and their counts and sizes include forgotten records.
- README studio section (`:19`, `:632`): studio is read-only unless started with `--allow-forget`.
- `atticdb-core/README.md:119-125`: the new tags and the forget sentence.
- 0003: fill I10's "Checked by" (0003:121). Fix the stale test names at 0003:115 and 0003:117.
- 0002: add D98 and D100. D99 lands with Task 4.
- Whole-feature review. Dogfood: forget a note from the CLI and from studio, then export the home.

## Order

"Depends on" means merged to `main`, not branched from. A task's branch starts from `main` after every PR it depends on has merged. Branching from an open task branch, or rebasing onto one, doesn't satisfy a dependency.
- Task 1 depends on nothing. It merges first.
- Task 2 depends on merged Task 1. It needs `enqueue_forget_sim` and the readers.
- Task 3 depends on merged Task 1. It needs `Engine::forget`, the placeholders, `forgotten_up_to` and `as_of_lsn`.
- Task 4 depends on merged Task 3, and so on Task 1. It needs `POST /v1/forget` and `as_of_lsn` in sessions and window JSON.
- Task 5 depends on merged Task 3, and so on Task 1. It needs the placeholders and edits the same `Route` list (`api.rs:42-83`).
- Task 6 depends on merged Tasks 1 to 5. It fills I10's "Checked by" from Task 2.
- Tasks 2 and 3 may run in parallel. Tasks 4 and 5 may run in parallel. Task 2 may still be open while Tasks 4 and 5 run.

## Out of scope

- Compaction and physical erase.
- Un-forget and `forget_status`.
- Retention.
- Markdown import.
- `/v1/records?kind=forget`.
- Studio export, studio LSN-list forget, and studio append or fact writes.
- Forget and export in the Python client.
- `source_refs`.
- Dropping hidden tokens from the live word index, and persisting it.
- Search snippets.

## Open questions (owner)

1. No `memory_forget` MCP tool in S5 (recommended)?
2. Keep fully forgotten sessions in `/v1/sessions`, with `forgotten_up_to` (recommended)? A name can be sensitive.
3. `max_forgotten_lsns` default of 100,000?
4. JSON envelope for export, with files cut into chunks by byte offset (recommended), or tar?
5. Partly settled: the name stays `redacted` (0003 I5, 0004:91). Still open: a `redacted: bool` on `Turn` and `Revision` (recommended), or an enum? Note that client write results already use `redacted` for secret counts (`client.rs:715-725`).
6. Accept and document guess confirmation through retry keys and `turn_repeats` (recommended), or drop retry entries for forgotten records?
7. Studio forget opt-in with `--allow-forget` (recommended after the pre-flight review), or on by default?
8. Studio session forget pinned to the `as_of_lsn` the view loaded at (recommended after the pre-flight review), or with no `up_to_lsn`?
9. Pre-S5 binaries fail replay with `corrupt_index` and lose nothing (recommended, docs only), or bump MANIFEST on the first forget as D95 did?

## Answered by main since 2026-10-30

- Studio forget is now in scope (owner, 2026-11-03). A studio export button stays out.
- File refs need no new selector: a ref is a fact key (D94).
- The index needs no delete path for I10: it only proposes candidates (D96).
- `source_refs` stays empty (`machine.rs:95`, `api.rs:440`), so D39's flag stays out.
- D89 and D90 are taken. Forget and export become D98 and D100.

## Decision rows to add (format of D94 to D97)

| D98 | The engine keeps a forget set per tenant: session and fact-key cutoffs, and hidden LSNs, all hash maps and sets with O(1) checks, checkpointed as tags 6 to 9. Every reader checks it before the hot cache, the tape or an inflate. Hidden turns read as `{seq, lsn, redacted: true}` and hidden revisions as `{lsn, redacted: true}`. Search skips them, and a forgotten current fact has no value. `max_forgotten_lsns` caps the LSN set at 100,000. No `memory_forget` MCP tool | A forget bit in each index entry. Removing hidden records from the word index. An MCP tool behind config, as 0001 planned | Checks stay O(1) and bounded (I5, I12). The index only proposes, so the check after it is enough (D96). An agent that misreads a request or follows an injected note could hide memory the user relies on, and forget can't be undone through the API |
| D99 | `atticdb studio` can forget, and nothing else. It amends D87. 1 write route, `POST /api/forget`, forwards each item as `POST /v1/forget` with the token the bridge holds: 1 to 100 sessions or 1 fact key, each with a retry key the page made. On top of D87's checks, a write needs `Origin` equal to the studio's own, `Sec-Fetch-Site` absent or `same-origin`, `Content-Type: application/json`, at most 64 KiB, and `"confirm": "hide"`. Other non-GET methods stay 405, and no CORS answer is ever sent. The page asks first, with the sentence "Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159)." and a link to what stays visible. Studio stays read-only by default: the route exists only with `atticdb studio --allow-forget`, and without it studio answers as today. Every item carries `up_to_lsn`, the `as_of_lsn` of the view the user saw, so later turns stay visible | Forget through `atticdb forget` only. Forget on by default with a `--read-only` opt-out. No `up_to_lsn` pin. A cookie session. A general write proxy for appends and facts | Owner ask 2026-11-03: users pick sessions in the table and forget them. Opt-in keeps a leaked studio URL read-only unless the user chose otherwise. The pin stops a stale table from hiding turns the user never saw. The secret travels in a custom header, never a cookie, and the JSON type forces a preflight that fails, so another origin can't send a write. Forget only, because it destroys no bytes and the log records it |
| D100 | Markdown export is `GET /v1/export?format=markdown`: paged JSON `{path, offset, markdown}` chunks pinned to 1 durable LSN. A page cuts inside a file, even inside 1 turn, so the whole body stays under `max_bytes` (64 KiB to 8 MiB), counted in encoded bytes. The cursor names the next entry and the bytes of it already sent. `atticdb export <dir>` appends chunks to `.part` files, renames each when done, and renames the folder last. Names are percent-encoded lowercase, with a hash past 200 bytes. Forgotten turns are placeholders and forgotten facts are left out. Export reads never fill the hot cache | A tar stream. 1 markdown file. Whole files per page | Responses need a `Content-Length` and a bound, so a big export pages. 1 session can outgrow any page, so pages cut inside files. JSON pages carry a cursor and file edges. A tar needs a hand-written writer and can't page. A single file is ambiguous to split |

---


## Rulings on the open questions (2026-11-03)

Taken on the owner's behalf so work can start. Each can change before its task merges.

- Q1: no `memory_forget` MCP tool. Forget runs from the CLI or studio.
- Q2: fully forgotten sessions stay in `/v1/sessions` with `forgotten_up_to`.
- Q3: `max_forgotten_lsns` defaults to 100,000.
- Q4: export uses the JSON envelope. Pages carry `{path, offset, markdown}` chunks cut inside files, so 1 huge session or `facts.md` still fits `max_bytes`. The client appends chunks by offset and renames at the end. Task 5 has the cursor and the byte rules. Amended after the pre-flight review: whole files per page broke the size bound.
- Q5: placeholders carry `redacted: true`.
- Q6: document that a retry key can confirm a guessed write. Keep retry entries.
- Q7: studio stays read-only by default. Only `atticdb studio --allow-forget` registers the forget route, which covers sessions and fact keys. Any later fact edit route sits behind the same flag. Without it, `/api/forget` answers as any unregistered path does today. Changed after the pre-flight review: a write route on by default widens what a leaked studio URL can do.
- Q8: a studio session forget sends the `as_of_lsn` the view loaded at as `up_to_lsn`, then refreshes the view. A turn written after the view loaded stays visible. Changed after the pre-flight review: with no pin, a click on a stale table also hides turns the user never saw.
- Q9: older binaries fail replay with `corrupt_index` and lose no data. Document it.

## Hide is not erase: the purge follow-up

Research on 12 memory tools (2026-11-03) found the most repeated complaint is "delete didn't really delete". S5 hides. It does not erase.
So every surface says so in plain words, with 1 sentence: "Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159)."
- Studio confirm: that sentence, then a link to "What a forget leaves visible" (Task 4).
- CLI output: that sentence, then a pointer to the same README section (Task 3).
- Docs: the README opens "Forget and export" with it and carries the list (Task 6).
Real erase (rewrite log segments without forgotten records, then drop the old files) is a separate plan. It is tracked in #159.
