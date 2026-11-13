# Search Ranking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `memory_search` returns turns and facts that match any query word, ranked by summed IDF, so a pasted question finds its evidence.

**Architecture:** The word index (`atticdb-core/src/search_index.rs`) already holds each token's postings. A new relevance mode scores LSNs from those postings in memory, then reads only the top ones through the existing visibility checks. Newest-first AND search stays as `order=newest`. MCP and the CLI default to relevance, and the HTTP API keeps newest as its default.

**Tech Stack:** Rust. Python for the LoCoMo harness.

**Spec:** GitHub issue Clawdlinux/atticdb#199, plus the owner's choices from 2026-11-13: MCP and CLI ranked by default, HTTP API newest by default; fixed English stopword list; ranked results are top-only with no next page.

## Global Constraints

- Scoring: `idf(w) = ln(1 + (N - df + 0.5) / (df + 0.5))`. `N` is the tenant's indexed record count. `df` is the number of distinct indexed LSNs holding `w` or its singular as a substring of a token. A record's score is the sum of `idf` over the query words it holds. Ties go to the higher LSN (newer).
- Query words: lowercase the query and split on every char that isn't `char::is_alphanumeric`. Keep words of 3 or more chars that aren't in `STOPWORDS`, deduplicated in first-seen order. If none are left, use every non-empty split word instead.
- `STOPWORDS` is the list in `research/locomo_retrieval/words.py`, copied exactly. It's never tuned on answers.
- Matching stays the same as today: a word, or its `singular`, is a substring of the record's lowercased text. A hit must hold at least 1 query word.
- Ranked results return at most `limit` hits and `next: null`. `before` still limits results to LSNs below it.
- Hidden turns, forgotten records and superseded or deleted facts never show. These are the same checks as newest mode.
- Performance: search p50 under 1 ms on the LoCoMo harness, on the same machine as the 2026-11-07 run.
- Prose (docs, comments, tool descriptions): no em dashes, sentences under 20 words, numbers as digits.
- No new dependencies. Commits use `git commit -s`, no Co-authored-by trailer.

## Rulings

- Ruling: score every query word, whatever its `df`. No word is skipped for being common. Why: the simulation scored higher without a skip, recall@10 59.5 against 58.4. A skip also breaks small stores. With 1 record, a word in it was never scored. Cost if wrong: very common words cost more to decode. Task 3 measures that.
- Ruling: no `score` field in hits. Why: no caller needs it yet. Cost if wrong: a later additive field.
- Ruling: records above the index watermark are scored by reading them, using `df` from the index. Those are the newest records, which the index hasn't reached yet. A word the index hasn't seen gets `df = 0`. Why: search already walks those records 1 by 1.

## Review Focus

1. A query of only stopwords or short words, such as `is it ok`, still returns matches.
2. A forgotten turn or a superseded fact never shows in ranked results, even when it scores highest.
3. `before` with ranked order returns only hits below that LSN.
4. A record above the index watermark ranks by the same score as an indexed one.
5. `order=newest` behaves exactly as search did before this change. Existing search tests pass unchanged.

---

### Task 1: Ranked search in the engine

**Files:**
- Modify: `crates/atticdb-core/src/search_index.rs` (per-tenant record count; per-word LSN set with `df`)
- Modify: `crates/atticdb-core/src/engine.rs` (`SearchQuery`, `search`)
- Create: `crates/atticdb-core/src/rank.rs` (`STOPWORDS`, `query_words`, `idf`), and register it in `lib.rs`
- Test: unit tests in `rank.rs` and `search_index.rs`. Engine tests go wherever existing `search` tests live (find them with grep).

**Interfaces:**
- Produces:
  - `pub enum SearchOrder { Newest, Relevance }`, exported like `SearchQuery`.
  - `SearchQuery` gains `pub order: SearchOrder`. Every existing constructor passes `SearchOrder::Newest`.
  - `pub(crate) fn query_words(query: &str) -> Vec<String>` and `pub(crate) fn idf(n: u64, df: u64) -> f64` in `rank.rs`.
  - On `Index`: `pub(crate) fn records(&self, tenant: u32) -> u64`, and `pub(crate) fn lsns(&self, tenant: u32, word: &str, stem: Option<&str>, below: u64) -> Vec<u64>`, which returns ascending distinct LSNs.

- [ ] **Step 1: Write failing tests**

`rank.rs`:
- `query_words("What did Caroline paint, in 2022?")` gives `["caroline", "paint", "2022"]`.
- `query_words("is it ok")` gives `["is", "it", "ok"]`.
- `idf(100, 1)` is greater than `idf(100, 50)`.

`search_index.rs`: after adding 3 records, `records` gives 3. `lsns` for a word in 2 of them gives those 2 LSNs, and `below` cuts them.

Engine:
- Relevance ranks a turn holding 2 rare query words above a newer turn holding 1.
- Equal scores go newest first.
- A forgotten turn and a superseded fact never show.
- `before` filters.
- `next` is `None`.
- A record written after the index caught up still shows with its score.
- The whole question `"When did Melanie paint a sunrise?"` finds a turn containing `sunrise`, which newest mode would miss.

- [ ] **Step 2: Run them, confirm they fail**

Run: `timeout 900 cargo test -p atticdb-core`
Expected: the new tests FAIL to compile or fail.

- [ ] **Step 3: Implement**

In `search`, branch on `query.order`. Relevance:
1. Take the words from `query_words`, and each one's `singular`.
2. Get `N` from `records`. Score every word, using `lsns` below `min(top, indexed) + 1`.
3. Walk the records above `indexed` the way search does today, and score them from their text.
4. Sort the candidates by score, then LSN, descending.
5. Visit them in that order through the same `pick` checks, with the match rule "holds at least 1 scored word". Stop at `limit`, `max_bytes` or `visits`.
6. Return `next: None`.

Keep newest mode byte-for-byte the same.

- [ ] **Step 4: Run tests, confirm they pass**

Run: `timeout 900 cargo test -p atticdb-core` and `cargo clippy -p atticdb-core --all-targets -- -D warnings`
Expected: all pass. The existing search tests are unchanged.

- [ ] **Step 5: Commit**

`git commit -s -m "feat(core): ranked search by summed IDF (#199)"`

### Task 2: `order` through the HTTP API, MCP and CLI

**Files:**
- Modify: `crates/atticdb/src/api.rs` (`search`: `order=newest|relevance`, default `newest`; anything else is 400 `invalid_input`)
- Modify: `crates/atticdb/src/client.rs` (`search`: sends `order=relevance` unless `args["order"] == "newest"`. Sends `before` in both orders.)
- Modify: `crates/atticdb/src/mcp.rs` (`memory_search` schema and description)
- Modify: the CLI `search` verb: `--order relevance|newest`, default relevance. Find the parser from `main.rs:119`.
- Modify: `crates/atticdb/README.md` (API table row for `/v1/search`, the search notes, the CLI usage), top-level `README.md` if it describes search order, `docs/design/0002-decision-log.md` (D113)
- Test: `crates/atticdb/tests/it/` (the existing search and MCP tests)

**Interfaces:**
- Consumes: `SearchOrder` and `SearchQuery.order` from Task 1.

- [ ] **Step 1: Write failing tests**

- HTTP: `order=relevance` ranks as in Task 1. No `order` keeps newest. `order=x` gives 400.
- MCP: `memory_search` with no `order` returns ranked hits. `order: "newest"` returns newest first with a `next` cursor.
- CLI: `atticdb search --query ...` ranks. `--order newest` doesn't.
- `tools/list` shows `order` with enum `["relevance", "newest"]` and default `relevance`.

- [ ] **Step 2: Run, confirm fail**

Run: `timeout 900 cargo test -p atticdb --test it search`

- [ ] **Step 3: Implement**

`memory_search` description, exactly:

`Find past turns and facts by words, across every session and agent. By default it ranks by relevance: a hit holds any query word, and rarer words count more. Pass a whole question if you like. order=newest needs every word and returns newest first, with before for paging. Use it when you don't know the fact key or the session.`

`order` schema: `{"type": "string", "enum": ["relevance", "newest"], "default": "relevance"}`.

- [ ] **Step 4: Run all tests**

Run: `timeout 900 cargo test -p atticdb` and `cargo clippy -p atticdb --all-targets -- -D warnings`. Then run `cargo build --release -p atticdb` and `ATTICDB_BIN=target/release/atticdb PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 PYTHONDONTWRITEBYTECODE=1 timeout 900 python3 -m pytest -q research`.
Expected: all pass. A research test that assumed newest-first MCP search gets `order: "newest"`, nothing more.

- [ ] **Step 5: Docs and commit**

D113: MCP and CLI search rank by summed IDF over any query word, with stopwords and a skip for words in over half the records. The HTTP default stays newest. Ranked results are top-only. Alternatives rejected:
- full BM25: term frequency and length made LoCoMo worse, and need an index format change.
- an offset cursor: results shift between calls.

It amends D86 and D96.

`git commit -s -m "feat: rank memory_search by relevance by default (#199)"`

### Task 3: Measure on LoCoMo

**Files:**
- Modify: `research/locomo_retrieval/arms.py`, `run.py` (new arm `atticdb-ranked`: the whole question, `order=relevance`)
- Modify: `research/locomo_retrieval/README.md`
- Create: `research/locomo_retrieval/results/2026-11-13.md` and `2026-11-13-summary.md`, in the same format as the 2026-11-07 files
- Test: `research/locomo_retrieval/tests`

**Interfaces:**
- Consumes: HTTP `order=relevance` from Task 2. The harness client is `research/atticdb_client` or whatever `arms._load_client` loads. Add an `order` argument to its `search` if it lacks one.

- [ ] **Step 1: Test.** A harness unit test: the `atticdb-ranked` arm sends the whole question with `order=relevance`. Existing arms send no `order`, so they stay newest.
- [ ] **Step 2: Run the benchmark** against the Task 2 release build, the same way the README says the 2026-11-07 run was made.
- [ ] **Step 3: Write results.** Report recall and hit at k 5, 10 and 20, plus p50 latency, for every arm. State plainly whether `atticdb-ranked` meets `fts5-bm25` recall@10 and stays under 1 ms p50. If it misses either, say so. Don't tune anything.
- [ ] **Step 4: Commit** `git commit -s -m "research(locomo): ranked search arm and 2026-11-13 results (#199)"`
