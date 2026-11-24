# Dated memory rule Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tell agents how to read a dated memory line that disagrees with the code, and prove it helps before release.

**Architecture:** 1 rule, 3 sentences, shipped in 2 places: the chat-start hook header and `mcp::INSTRUCTIONS`. `connect` already copies `INSTRUCTIONS` into the Copilot, Codex and Gemini instruction files. The `research/stale_rule` harness gets 1 arm for that instruction-file channel, then we rerun it.

**Tech Stack:** Rust (crate `atticdb`), Python 3 stdlib harness, `claude -p` for the eval.

**Spec:** [#233](https://github.com/Clawdlinux/atticdb/issues/233). Evidence: [research/stale_rule/results/2026-11-15-results.md](../../research/stale_rule/results/2026-11-15-results.md).

## Global Constraints

- The rule text, verbatim, everywhere: `If a dated line disagrees with the code, check git log for that code. Code changed after the line's date means the line is stale: follow the code. Code older than the line means the line is the new direction: follow the line.`
- In the header it goes right after `check it against the repo.` and before `If it is wrong, set its key again.`
- No commit anchors on facts. The ablation showed they add nothing with the date and sentence, and mislead alone (3/8 migrations).
- No "existing code wins" wording. It broke every migration (0/12).
- No new dependencies. Writing rules from `CONTRIBUTING.md`. Commits use `git commit -s`, no `Co-authored-by`.
- The hook block stays within `MAX_BLOCK` (6,000 chars).

## Review Focus

- `INSTRUCTIONS` is at its 275-word cap now. Adding 43 words must raise the budget on purpose, with the reason in the test's doc comment. Not by trimming other rules silently.
- Hosted sessions get `hosted_instructions()`, which strips local-only sentences. The rule must survive that strip.
- Users who already ran `connect` have the old guide file. `connect` rewrites a marked file whose bytes differ, and `disconnect` deletes only an exact match. Rerunning `atticdb setup` or `connect` must update it, not skip it.
- The harness `atticdb-today` arm must stay the OLD header, so the rerun still has a baseline. Only `NEXT_HEADER` should match the new shipped header.
- Release gate: if the eval doesn't clear the bar in Task 3, the header change doesn't ship as is. Report the numbers on #233 instead.

---

### Task 1: Ship the rule in the header and the guide

**Files:**
- Modify: `crates/atticdb/src/hook.rs` (header `format!`, about line 276)
- Modify: `crates/atticdb/src/mcp.rs` (`INSTRUCTIONS`, about line 324)
- Modify: `crates/atticdb/README.md:950` (header mirror)
- Test: `crates/atticdb/tests/it/hook.rs` (`header()`, line 56)
- Test: `crates/atticdb/tests/it/mcp.rs` (`instructions_stay_within_word_budget`, line 1592)

**Interfaces:**
- Produces: `pub const DATED_RULE: &str` in `crates/atticdb/src/hook.rs`, exactly the rule text above. The header interpolates it as `{DATED_RULE}`, as it does `{TURNS}`.
- `mcp::INSTRUCTIONS` holds the same text as a literal, right after `The block holds only the newest notes.`

- [ ] **Step 1: Write the failing tests**

In `tests/it/hook.rs`, update `header()` to include the rule after `check it against the repo.`.

In `tests/it/mcp.rs`, add:

```rust
/// A dated memory line can be stale or the new direction. Only history tells (#233).
#[test]
fn instructions_say_how_to_read_a_dated_line() {
    for instructions in [atticdb::mcp::INSTRUCTIONS.to_owned(), atticdb::mcp::hosted_instructions()] {
        assert!(instructions.contains(atticdb::hook::DATED_RULE), "{instructions}");
    }
}
```

Raise the budget in `instructions_stay_within_word_budget` to `240 + 35 + 43` (318). Update its doc comment: "#233 added 43 words: the rule for a dated line that disagrees with the code."

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --locked -p atticdb --test it -- hook::session_start_prints_the_memory_block mcp::instructions_say_how_to_read_a_dated_line`
Expected: FAIL. `DATED_RULE` not found, and the header mismatch.

- [ ] **Step 3: Implement**

Add `DATED_RULE` (with a 1-line doc comment citing #233), interpolate it in the header, paste the literal into `INSTRUCTIONS`, and update the README mirror line. If `hook` isn't public from the crate root, expose only the const.

- [ ] **Step 4: Run the crate tests, fmt and clippy**

Run: `cargo fmt --check && cargo clippy --locked -p atticdb --all-targets -- -D warnings && cargo test --locked -p atticdb`
Expected: all pass. Includes the `connect` and `setup` guide tests, which build from `INSTRUCTIONS`.

- [ ] **Step 5: Commit**

```bash
git add crates/atticdb/src/hook.rs crates/atticdb/src/mcp.rs crates/atticdb/README.md crates/atticdb/tests/it/hook.rs crates/atticdb/tests/it/mcp.rs
git commit -s -m "feat(hook): tell agents how to read a dated line against git (#233)"
```

### Task 2: Add the guide arm to the harness

**Files:**
- Modify: `research/stale_rule/fixtures.py`
- Test: `research/stale_rule/tests/test_fixtures.py`

**Interfaces:**
- Consumes: `NEXT_SENTENCES`, `NEXT_HEADER`, `memory_block()`, `memory_user_file()`.
- Produces: arm `atticdb-next-guide` in `CANDIDATE_ARMS`. Its block is `NEXT_HEADER` plus the dated line. Its user `CLAUDE.md` is `NEXT_SENTENCES.strip() + "\n"`. That is the shipped rule arriving through the instruction-file channel, as the Codex, Gemini and Copilot guides deliver it.

- [ ] **Step 1: Write the failing test**

```python
def test_guide_arm_ships_the_rule_twice(tmp_path):
    conv = fixtures.CONVENTIONS["errors"]
    repo = str(tmp_path / "p")
    fixtures.build(conv, "stale", repo, channel="memory", neutral=True)
    block = fixtures.memory_block("atticdb-next-guide", conv, "stale", repo)
    assert block.startswith(fixtures.NEXT_HEADER)
    assert "(2026-03-02)" in block
    assert fixtures.memory_user_file("atticdb-next-guide") == fixtures.NEXT_SENTENCES.strip() + "\n"
    assert "atticdb-next-guide" in fixtures.CANDIDATE_ARMS
```

Also assert the shipped text matches: `NEXT_SENTENCES.strip()` equals the rule text in Global Constraints.

- [ ] **Step 2: Run it to verify it fails**

Run: `PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest -q research/stale_rule/tests`
Expected: FAIL on `ValueError: atticdb-next-guide`.

- [ ] **Step 3: Implement** the arm in `memory_block()` and `memory_user_file()`, and add it to `CANDIDATE_ARMS`.

- [ ] **Step 4: Run the suite**

Run: `PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest -q research/stale_rule/tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add research/stale_rule/fixtures.py research/stale_rule/tests/test_fixtures.py
git commit -s -m "research(stale_rule): add the guide-channel arm for #233"
```

### Task 3: Rerun the eval and gate the release

**Files:**
- Create: `research/stale_rule/results/2026-11-24-dated-rule.csv`
- Modify: `research/stale_rule/results/2026-11-15-results.md` (new section 4)

- [ ] **Step 1: Smoke run, 1 cell**

Run: `timeout 900 python3 -m research.stale_rule run --batch 2026-11-24-smoke --channel memory --neutral --reps 1 --jobs 1 --conventions errors --arms atticdb-next-guide --model claude-sonnet-5-5 --out /tmp/stale-smoke.csv`
Expected: 2 rows, `error` empty, the recorded model `claude-sonnet-5-5`. If login fails, stop and report. Don't work around it.

- [ ] **Step 2: Full run, neutral history, 3 reps**

Run: `timeout 7200 python3 -m research.stale_rule run --batch 2026-11-24-dated-rule --channel memory --neutral --reps 3 --jobs 4 --arms atticdb-today atticdb-next atticdb-next-guide --model claude-sonnet-5-5 --out research/stale_rule/results/2026-11-24-dated-rule.csv`
Expected: 72 graded rows. Rerun errored cells with `--resume`. About $4 at API prices, from the 2026-11-15 cost per run.

- [ ] **Step 3: Gate**

Run: `python3 -m research.stale_rule summary research/stale_rule/results/2026-11-24-dated-rule.csv`
Ship if `atticdb-next-guide` gets stale >= 10/12 and migration >= 11/12, and `atticdb-next` doesn't drop migrations below 11/12. Otherwise keep Task 1 unmerged and post the table on #233.

- [ ] **Step 4: Write up and commit**

Add section 4 to the results file: the table, cost, Claude Code version, and limits. Claude Code reads `INSTRUCTIONS` from MCP initialize, not a user file, so this arm stands in for it. Say so.

```bash
git add research/stale_rule/results/2026-11-24-dated-rule.csv research/stale_rule/results/2026-11-15-results.md
git commit -s -m "research(stale_rule): rerun with the dated-line rule (#233)"
```

### Task 4: PR

- [ ] Open 1 PR with Tasks 1 to 3, using `.github/pull_request_template.md`. `Closes #233` only if the gate passed. Add a decision log row in `docs/design/0002-decision-log.md` for the new rule.

## What this does NOT do

- No commit anchors, no `N commits since` counts, no new fields on facts.
- No change for apps that skip both the hook and the guide.
- No eval on Codex or Copilot CLI. Codex quota is out until Nov 29 (#190).
- Doesn't touch #234, the `(cut)` fact test. That's the next plan.
