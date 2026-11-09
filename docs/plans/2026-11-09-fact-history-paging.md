# Fact history paging (#206). Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An agent can read all of 1 fact's history, not just the newest 16 revisions. It pages with a `before` cursor.

**Architecture:**
- `fact_history` reads the in-memory index as today, and now also returns `next`, the cursor for older revisions.
- A new engine method continues below `next` with the same newest-first tape walk that `records` uses, keeping only this key's revisions.
- HTTP, MCP and the CLI each take an optional `before` and pass `next` through.
- No format change.

**Tech Stack:** Rust (atticdb-core, atticdb). No new dependencies.

**Spec:** This plan's Design section, which the owner approved on 2026-11-09. Issue #206 is the background.

## Design (approved 2026-11-09)

- `FactHistory` gains `next: Option<u64>`.
  - The index read sets it to the LSN of the oldest revision returned when older revisions exist or the budget cut the reply. In both cases `truncated` is true.
  - Otherwise `next` is `None`. `truncated` stays, for older clients.
- `Engine::fact_history_before(tenant, key, before, max_bytes, visits)` walks the tenant's tape below `before`, newest first, through `newest_first`, and keeps only this key's fact revisions.
  - Page size is the store's `fact_history` setting (default 16).
  - `next` follows `newest_first`'s rules: after `limit` items, after an over-budget item, or after `visits`. A page can be empty with `next` set.
  - Forgotten revisions show as redacted placeholders, as in `records`.
- `GET /v1/facts/history` takes an optional `before=<lsn>`. With it, the route calls `fact_history_before` with `SEARCH_VISITS`. The reply always carries `next`.
- MCP `fact_history` takes optional `before`, and `atticdb fact history` takes `--before <lsn>`. When `next` is set, the last text line is `more: call fact_history again with before=<next>`. It replaces the old "raise max_bytes" hint.
- **Approach:** B (a cursor on fact history), not a `key` filter on `/v1/records`. That's 1 endpoint and 1 cursor for 1 question.
- **Not in scope:** studio paging, tape compaction, a per-key back-pointer.

## Global Constraints

- Follow existing patterns. Reuse `newest_first` and the fact branch of `records`. Don't add a second walk.
- No format change and no new dependency. Old clients keep working: `truncated` and the reply shape stay, and `next` is only added.
- The page size for a walk is the store's `fact_history` setting. The visit bound over HTTP is `SEARCH_VISITS`.
- Prose: no em dashes, sentences under 20 words, numbers as digits. Commits: `git commit -s`, no Co-authored-by.

## Review Focus

1. **A key with exactly 16 revisions:** `truncated` is false and `next` is `None`, so no empty page is offered. Test in Task 1.
2. **Other tenants' revisions of the same key between ours:** they never show. Test in Task 1.
3. **A forgotten revision below the cap:** it shows as a redacted placeholder with its LSN, not skipped. Test in Task 1.
4. **The visit bound hit before any match:** an empty page with `next` set, and following `next` reaches every revision. Test in Task 1.
5. **`before` that isn't a whole number, over HTTP:** 400 `invalid_input`, like `/v1/records`. Test in Task 2.

---

### Task 1: Engine: `next` on history and `fact_history_before`

**Files:**
- Modify: `crates/atticdb-core/src/engine.rs`:
  - `FactHistory` (line 117)
  - `fact_history` (line 1325)
  - `records` (line 1533): factor its `Kind::Fact` branch into 1 private helper that both methods call
- Test: `crates/atticdb-core/tests/it/facts.rs`

**Interfaces:**
- Produces: `FactHistory { revisions, truncated, next: Option<u64> }`
- Produces: `pub fn fact_history_before(&self, tenant: &str, key: &str, before: u64, max_bytes: usize, visits: usize) -> Result<FactHistory>`
  - `truncated == next.is_some()` on its result.
  - It rejects `visits == 0` like `records` does.

- [ ] **Step 1: Write the failing tests** in `crates/atticdb-core/tests/it/facts.rs`. Use that file's `open`, `options` and `new_fact` helpers.

```rust
#[test]
fn history_pages_past_the_index_cap() {
    // Tenant "t": put key "k" 20 times (contents "v0".."v19"). Between puts, write 1 turn and
    // 1 put of key "other". Tenant "u": put key "k" 3 times.
    // first = fact_history("t", "k", usize::MAX): 16 revisions "v19".."v4", truncated, and
    //   next == Some(lsn of "v4").
    // rest = fact_history_before("t", "k", first.next.unwrap(), usize::MAX, usize::MAX):
    //   ["v3","v2","v1","v0"], next == None, truncated == false.
    // first.revisions ++ rest.revisions has 20 LSNs, strictly decreasing, all tenant t key k.
}

#[test]
fn history_at_the_cap_offers_no_next_page() {
    // 16 puts of "k": fact_history -> 16 revisions, truncated false, next None.
}

#[test]
fn history_walk_respects_visits_and_forgets() {
    // 20 puts of "k" with 50 turns between each. Forget the LSN of "v2".
    // Follow fact_history_before with visits = 10 from the first page's next until next is None.
    // At least 1 page is empty with next set. Collected LSNs equal the 4 oldest revisions' LSNs.
    // The "v2" revision has redacted == true.
}

#[test]
fn history_walk_budget_cut_resumes_at_the_cut_revision() {
    // 20 puts of "k", each 100 bytes. fact_history_before(.., before = first.next, max_bytes = 150, ..)
    // returns 1 revision ("v3"), next == Some(lsn_v2 + 1). The next call returns "v2" first.
}
```

- [ ] **Step 2: Run, expect FAIL (missing method and field):** `timeout 600 cargo test -p atticdb-core --test it facts::history_`
- [ ] **Step 3: Implement.**
  - `fact_history`: set `next` to the oldest returned revision's LSN when `cut || truncated`, else `None`.
  - `fact_history_before`: call `newest_first` with a header filter for `Kind::Fact`. Use a `pick` that decodes the key and returns `None` for other keys, and builds the `Revision` through the shared helper. `limit` is `state.facts.history`. Return `truncated: next.is_some()`.
  - `records` uses the same helper for its fact branch, with no behavior change.
- [ ] **Step 4: Run the new tests and the existing `facts::`, `search::` and `engine::` tests, plus `cargo test -p atticdb-core`. Expect PASS.**
- [ ] **Step 5: Commit:** `git commit -s -m "feat(core): page a fact's history past the index cap"`

### Task 2: HTTP, MCP, CLI and docs

**Files:**
- Modify: `crates/atticdb/src/api.rs`, `fact_read` (line 655): accept `before` only for `Route::FactHistory`, and reply with `next`.
- Modify: `crates/atticdb/src/client.rs`, `fact_history` (line 550) and its renderer (around line 996): pass `before`, and print the new hint.
- Modify: `crates/atticdb/src/mcp.rs`: add `"before": {"type": "integer", "minimum": 1}` to the `fact_history` schema. Add 1 sentence to its description: `When more revisions exist, call again with before set to next.`
- Modify: `crates/atticdb/src/main.rs`: the usage line and wiring for `--before`.
- Modify: `crates/atticdb/README.md`:
  - HTTP table rows for `/v1/facts/put`, `get`, `list`, `history` and `delete`. Use the shapes in `api.rs`.
  - The `fact_history` text in the MCP section.
- Modify: `docs/design/0002-decision-log.md`: new row D108.
- Test: `crates/atticdb/tests/it/mcp.rs`, plus the HTTP test file where facts routes are tested (find it with `grep -rln "facts/history" crates/atticdb/tests`).

**Interfaces:**
- Consumes: Task 1's `FactHistory.next` and `Engine::fact_history_before`.
- Produces: `GET /v1/facts/history?key=&max_bytes=&before=`, which replies `{"revisions", "truncated", "next"}`. MCP and CLI `before` arguments.

- [ ] **Step 1: Write the failing tests.**
  - **HTTP:** 20 puts of `k`. Following `next` from the first page reaches all 20 LSNs. `before=x` gives 400 `invalid_input`. `next` is `null` for a key with 3 revisions.
  - **MCP:** 20 `fact_set` calls on `repo/p` from 1 process. The first `fact_history` text ends with `more: call fact_history again with before=<n>`, where `<n>` equals `structuredContent.next`. Calling with `before: n` returns 4 revisions, and its text has no `more:` line.
  - **CLI:** `atticdb fact history --key repo/p --before <n>` prints JSON with 4 revisions.
- [ ] **Step 2: Run, expect FAIL:** `timeout 900 cargo test -p atticdb --test it -- fact_history`
- [ ] **Step 3: Implement** the files above, following the existing `records` and `fact_list` handling for `before` and `next`.
- [ ] **Step 4: Add D108:** "`fact_history` returns `next`, and `before` pages below the index cap. It uses the tenant's newest-first tape walk, keeping 1 key's revisions, with no format change. Why: agents asked how far back 1 key's history goes (#206). A per-key back-pointer waits until scan cost shows up in real use."
- [ ] **Step 5: Full checks, expect PASS.**
  - Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && timeout 1500 cargo test --workspace`
  - The known flaky test (#201) may fail once. Rerun it alone.
  - Then: `PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 PYTHONDONTWRITEBYTECODE=1 python3 -m pytest research -q`
- [ ] **Step 6: Commit:** `git commit -s -m "feat: fact_history pages with before and next over HTTP, MCP and CLI"`
