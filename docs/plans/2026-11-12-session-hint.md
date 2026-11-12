# Session Hint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Agents save notes to the repo they work in, and the chat-start block shows which other sessions hold notes.

**Architecture:** Task 1 changes 1 sentence of `INSTRUCTIONS` in `crates/atticdb/src/mcp.rs`, inside the word budget. Task 2 adds 1 line to the block built in `crates/atticdb/src/hook.rs`, from the existing `GET /v1/sessions` route.

**Tech Stack:** Rust, serde_json.

**Spec:** GitHub issue Clawdlinux/atticdb#220.

## Global Constraints

- Prose (guide, docs, comments, block text): no em dashes, sentences under 20 words, numbers as digits.
- `INSTRUCTIONS` and `hosted_instructions()` stay at or under 275 words (`tests/it/mcp.rs` `instructions_stay_within_word_budget`).
- The block stays byte-identical until memory changes. No times, counts or "N ago" in the new line.
- The block stays under `MAX_BLOCK` (6,000) and keeps the `(more in atticdb)` cut.
- No new dependencies, no new server routes. Commits use `git commit -s`, no Co-authored-by trailer.

## Rulings

- Ruling: the new block line lists session names only, not counts or dates. Why: `turns` counts forgotten turns too, and dates need 1 read per session. Cost if wrong: a session whose notes are all forgotten still shows by name.
- Ruling: a failed sessions read leaves the line out and keeps the rest of the block. Why: the line is a hint, not memory. Cost if wrong: a broken route goes unnoticed in the block.

## Review Focus

1. A store whose only session is the current repo: no new line.
2. The same session name under several agents: listed once.
3. More than 20 other names: the first 20 in byte order, then `and <N> more`.
4. The sessions read fails: the block is otherwise unchanged.
5. An empty session name never shows.

---

### Task 1: The guide defines `<repo>` as the repo you work in

**Files:**
- Modify: `crates/atticdb/src/mcp.rs` (`INSTRUCTIONS`)
- Modify: `docs/design/0002-decision-log.md` (add D112 after D111, same table format)
- Test: `crates/atticdb/tests/it/mcp.rs`

**Interfaces:**
- Produces: nothing other tasks use.

- [ ] **Step 1: Write the failing test**

In `tests/it/mcp.rs`, add `instructions_define_repo_as_the_work_repo`: assert `INSTRUCTIONS` contains exactly `<repo> is the folder name of the repo you work in, not always the chat's.` and does not contain `main checkout's folder name`. Assert the same for `hosted_instructions()`.

- [ ] **Step 2: Run it, confirm it fails**

Run: `timeout 900 cargo test -p atticdb --test it instructions_`
Expected: the new test FAILS. The budget test passes.

- [ ] **Step 3: Edit `INSTRUCTIONS`**

- Replace `<repo> is the main checkout's folder name, as in the block.` with `<repo> is the folder name of the repo you work in, not always the chat's.`
- Replace `agent/<agent>/<topic> holds notes for 1 app only, and chat start doesn't load it yet.` with `agent/<agent>/<topic> is for 1 app only, and chat start doesn't load it.`
- Change nothing else. The 2 edits net +2 words, from 271 to 273.

- [ ] **Step 4: Run the tests, confirm they pass**

Run: `timeout 900 cargo test -p atticdb --test it` and `cargo clippy -p atticdb --all-targets -- -D warnings`
Expected: all pass. Any other test that pins the old sentences gets updated to the new text, nothing more.

- [ ] **Step 5: D112 and commit**

D112: the guide defines `<repo>` as the repo the agent works in, not always the chat's folder, because chats opened in 1 repo filed another repo's notes under the wrong session (#220). Alternative rejected: inferring the session on the server from file paths.

```bash
git commit -s -m "fix(mcp): define <repo> as the repo the work is in (#220)"
```

### Task 2: The block names other sessions with notes

**Files:**
- Modify: `crates/atticdb/src/client.rs` (add a method on the hook's reader, next to `forgotten_up_to`)
- Modify: `crates/atticdb/src/hook.rs` (`block_in`)
- Modify: `crates/atticdb/README.md` (the `atticdb hook session-start` section)
- Modify: `docs/design/0002-decision-log.md` (extend D112 with 1 sentence for the block line)
- Test: `crates/atticdb/src/hook.rs` `mod tests`, `crates/atticdb/tests/it/hook.rs`

**Interfaces:**
- Consumes: `GET /v1/sessions?limit=<n>&after_agent=<a>&after_session=<s>`, reply `{"sessions": [{"agent", "session", "turns", "forgotten_up_to"}], "next": {"agent", "session"} | null, ...}`. Read it the way `forgotten_up_to` in client.rs does (`self.client.read("hook", &target, "sessions", ...)`).
- Produces: `pub(crate) fn session_names(&mut self) -> Result<Vec<String>, String>` on the same reader type as `forgotten_up_to`: distinct non-empty session names across all agents, sorted by byte order. Pages at `limit=1000`, follows `next`, stops after 5 pages.

- [ ] **Step 1: Write the failing tests**

Unit test a pure helper `others(names: Vec<String>, repo: &str) -> Option<String>` in hook.rs:
- `["app"]` with repo `app` gives `None`.
- `["b", "a", "app", "a", ""]` with repo `app` gives `Some("Other sessions with notes: a, b. When you work in one of those repos, pass its name as session and call fact_list with prefix repo/<name>/.")`.
- 23 other names give the first 20 in byte order, then `, and 3 more` before the period after the list.

Integration test in `tests/it/hook.rs`: seed notes in sessions `app` and `other` (2 agents for `other`), run the hook in `app`, and assert the block's second line is the `others` line naming `other` once. Update the existing exact-block tests: with no other sessions, no line.

- [ ] **Step 2: Run them, confirm they fail**

Run: `timeout 900 cargo test -p atticdb hook`
Expected: the new tests FAIL.

- [ ] **Step 3: Implement**

- `session_names` in client.rs as in Interfaces.
- `others` in hook.rs as in Step 1, with the exact text there.
- In `block_in`, after all other reads, call `home.session_names()`. On `Err`, leave the line out. Insert the line directly after the header, before `Rules:`, so the `MAX_BLOCK` cut never drops it.

- [ ] **Step 4: Run tests, confirm they pass**

Run: `timeout 900 cargo test -p atticdb` and `cargo clippy -p atticdb --all-targets -- -D warnings`
Expected: all pass. Also run `ATTICDB_BIN=target/release/atticdb PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 PYTHONDONTWRITEBYTECODE=1 timeout 900 python3 -m pytest -q research/setup_eval/tests research/handoff_eval/tests` after `cargo build --release -p atticdb`. Their graders parse the block.

- [ ] **Step 5: Docs and commit**

README: add the line to the `text` example, and 1 sentence: it names other sessions with notes, at most 20, by name only, and is left out when there are none or the read fails. D112: 1 more sentence for the block line.

```bash
git commit -s -m "feat(hook): name other sessions with notes in the block (#220)"
```
