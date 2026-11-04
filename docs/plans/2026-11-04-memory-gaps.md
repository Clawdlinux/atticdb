# Memory gaps from the handoff pilot: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Fix the stage the 2026-11-04 pilot measured as failing for AtticDB: capture quality. Agents save the decision but drop its reason. Then rerun the same matrix and compare.

**Spec:** `research/handoff_eval/results/2026-11-04-pilot.md`, section "Hand audit and diagnosis", on branch `research/handoff-eval` (#178). The rule of `docs/plans/2026-11-04-handoff-eval.md` Task 5: fix the stage that fails, not the one we guess.

## Global Constraints
- No storage, engine or API change. The pilot shows delivery is 9/9 and storage is not the failing stage.
- `INSTRUCTIONS` (crates/atticdb/src/mcp.rs:225) goes into every chat-start guide. It may grow by at most 30 words net.
- Writing: no em dashes, sentences under 20 words.
- `git commit -s`, no Co-authored-by trailer.

## Evidence
- The current guidance says: "memory_append: 1 short note on what changed, PR or issue numbers and open follow-ups." It never asks for the reason.
- `fact_set`'s description doesn't ask for one either.
- Pilot H1, atticdb arm, Copilot to Claude: the writer saved `LEDG-7731` but not "412 vs 24 bytes". The reader correctly said the notes don't record why.

### Task A: capture reasons with their evidence (product, branch `feat/capture-reasons` from main)

**Files:** `crates/atticdb/src/mcp.rs` (INSTRUCTIONS, `memory_append` and `fact_set` descriptions), the tests that pin these strings, and the README section that quotes them, if one exists.

- [ ] Change the memory_append sentence so the note covers what changed AND why: the reason with its evidence (numbers, ticket or incident ids, the constraint), any approach that was rejected, PR or issue numbers, and open follow-ups.
- [ ] Add 1 sentence: when a decision changes, save what changed, why and the change id. Don't just delete the old one.
- [ ] `fact_set` description: put the reason in the content. Example: `Use struct for headers, not JSON: 24 vs 412 bytes (LEDG-7731).`
- [ ] `memory_append` description: "Save one note: what changed and why, with its evidence. Returns its sequence number."
- [ ] Update every test that asserts these strings. Add 1 test that the INSTRUCTIONS word count stays within the +30 budget.
- [ ] fmt, clippy -D warnings and `cargo test -p atticdb` pass. Commit.

### Task B: graders and a capture-quality metric (eval, branch `research/handoff-eval`)

**Files:** `research/handoff_eval/grade.py`, `run.py`, `results.py` and their tests.

- [ ] H5 abstain markers add "couldn't find", "could not find" and "couldn't confirm". Each must be followed by a retention or record phrase, and no number with a time unit may appear after it. Add the 4 pilot H5 replies as pass rows (they're in `results/2026-11-04-pilot.csv`).
- [ ] H4: a no that ties 3.9 to the handoff ("HANDOFF.md", "the notes", "memory", "last session") counts like 3.9 tied to CI. Add the pilot reply "No. HANDOFF.md requires Python 3.9 compatibility; structural pattern matching requires Python 3.10+." as a pass row.
- [ ] New per-row column `captured_all`: True when every evidence token of the case was found in a single capture location. For H1 that means `LEDG-7731` and `412` together. The summary adds a "Captured with reason" funnel row per arm.
- [ ] The CSV stores the full reader reply in a new `reply` column, not cut short. `python3 -m research.handoff_eval --regrade <csv>` re-grades stored replies with the current graders and prints the summary. It runs no agents.
- [ ] Both suites pass. Commit.

### Task C: rerun and compare (controller, after the Claude limit resets at 18:10 IST)
- [ ] Build atticdb from `feat/capture-reasons`.
- [ ] Run the atticdb arm, all 6 cases, both directions, 2 reps, with the same model pins. Run the same arm on the main binary (2 reps), plus H6 Copilot to Claude for all 4 arms on main (1 rep).
- [ ] Compare "Captured with reason" and pass rates, old vs new. Write `results/2026-11-04-rerun.md`. Merge Task A only if capture-with-reason doesn't drop and pass rates don't drop.
