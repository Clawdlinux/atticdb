# Fact Dates and Cut Markers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The chat-start block shows when each fact and note was saved, marks every cut line, and tells the agent to recheck dated facts against the repo.

**Architecture:** All changes live in `crates/atticdb/src/hook.rs`, where the block is built. Dates come from each record's `commit_time_ns`, formatted with the existing `crate::export::utc`. The recheck rule goes in the block header, because `INSTRUCTIONS` is at 271 of its 275-word budget.

**Tech Stack:** Rust, serde_json.

**Spec:** GitHub issue Clawdlinux/atticdb#217, with the deviation in the rulings below.

## Global Constraints

- Prose (docs, comments, header text): no em dashes, sentences under 20 words, numbers as digits.
- Dates are absolute UTC dates, `YYYY-MM-DD`. Never "N days ago": the block must stay byte-identical until memory changes, so the prompt prefix caches (README: "The block depends only on the repo and memory").
- Fact line format: `- <key> (<YYYY-MM-DD>): <content>`.
- Note line format: `- <YYYY-MM-DD>: <content>`.
- A record with no `commit_time_ns`, or 0, keeps today's format with no date part.
- A content longer than 300 characters (`MAX_CONTENT`) is cut to 300 characters, then ` (cut)` is appended. ` (cut)` is the marker the compaction summary already uses (hook.rs `MAX_SUMMARY`).
- Content at or under 300 characters gets no marker.
- `MAX_BLOCK` (6,000) and `(more in atticdb)` behavior is unchanged.
- No new dependencies. Commits use `git commit -s`, no Co-authored-by trailer.

## Rulings

- Ruling: the recheck rule goes in the block header, not `INSTRUCTIONS`. Why: `INSTRUCTIONS` is 271/275 words and the budget test pins it. The header is where the dates show. Cost if wrong: clients with no chat-start hook (Claude Desktop) don't get the rule. They don't see dates either.
- Ruling: notes get dates too. Why: the stale "next step" lines in the source audit were notes as well as facts. Costs 12 characters per note, 5 notes.

## Review Focus

1. A fact or note exactly 300 characters long: no ` (cut)`.
2. A 301-character multibyte content (`é`): cut at 300 characters, not bytes, then ` (cut)`.
3. A record whose `commit_time_ns` is missing: no date, no panic.
4. A block over 6,000 characters still stops at a whole line with `(more in atticdb)`. The longer lines must not break that.
5. Two hook runs with unchanged memory print byte-identical blocks.

---

### Task 1: Dates, cut marker and recheck rule in the chat-start block

**Files:**
- Modify: `crates/atticdb/src/hook.rs` (`fact`, `line`, the notes `map` in `block_in`, the header in `block_in`)
- Modify: `crates/atticdb/tests/it/hook.rs` (the exact-block test near line 108, and the `repo/big/` test near line 290)
- Modify: `crates/atticdb/README.md` (the `atticdb hook session-start` section, around lines 915-945)
- Modify: `docs/design/0002-decision-log.md` (add D111 after D110, same table format)

**Interfaces:**
- Consumes: `crate::export::utc(ns: u64) -> String` (returns `2026-11-03T13:17:17Z`; take the first 10 characters).
- Produces: nothing other tasks use.

- [ ] **Step 1: Write failing tests**

In the exact-block test, every fact line becomes `- <key> (<date>): <content>` and every note line `- <date>: <content>`. The test seeds records at real time, so compute each expected date from the record's `commit_time_ns` (read back with `atticdb fact get` / the append reply), not from the wall clock. The 3 contents seeded at over 300 characters (`rule_1`, `rule_2`, the `é` fact, `note`) now end in `" (cut)"`.

Add the header sentences, exactly, after `call memory_search.`:
`Dates show when each line was saved. Before acting on a fact about code, a version or a status, check it against the repo. If it is wrong, set its key again. A line ending in (cut) was shortened. Call fact_get for the rest.`

Add unit tests in `hook.rs` `mod tests` for `line`: 300 characters gives no marker; 301 `é` gives 300 `é` plus ` (cut)`; newlines still become spaces. Add one for `fact` with no `commit_time_ns`: `- k: v`.

Add a test that 2 hook runs on the same memory print the same block.

- [ ] **Step 2: Run tests, confirm they fail**

Run: `cargo test -p atticdb hook`
Expected: the changed tests FAIL on the missing dates, markers and header.

- [ ] **Step 3: Implement**

- `line(text: &Value, max: usize) -> String`: unchanged cut and newline handling, plus ` (cut)` when the content had more than `max` characters. `line(&fact["key"], usize::MAX)` must never get a marker.
- `fact(fact: &Value) -> String`: insert ` (<date>)` after the key when `commit_time_ns.as_u64()` is a nonzero value.
- Notes `map` in `block_in`: `- <date>: <content>` under the same rule.
- Header: append the sentences from Step 1.

- [ ] **Step 4: Run tests, confirm they pass**

Run: `cargo test -p atticdb hook` then `cargo test -p atticdb` and `cargo clippy -p atticdb --all-targets -- -D warnings`
Expected: all pass, no warnings.

- [ ] **Step 5: Docs**

README hook section: update the `text` example to the new line formats and header. Replace "Each content is cut to 300 characters" with a sentence saying a longer content is cut to 300 characters and ends in ` (cut)`. Add 1 sentence: dates are UTC save dates, absolute so the block stays the same until memory changes.

D111 row: the block shows each fact's and note's UTC save date, marks cut lines with ` (cut)`, and its header asks the agent to recheck code, version and status facts against the repo. Alternatives column: relative ages (rejected: break prompt caching) and a line in `INSTRUCTIONS` (rejected: 271/275 word budget). Reference #217.

- [ ] **Step 6: Commit**

```bash
git add crates/atticdb/src/hook.rs crates/atticdb/tests/it/hook.rs crates/atticdb/README.md docs/design/0002-decision-log.md
git commit -s -m "feat(hook): date facts and notes, mark cut lines (#217)"
```
