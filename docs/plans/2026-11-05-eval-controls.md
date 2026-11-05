# Eval controls: blank reader, AtticDB alone, same-harness handoffs. Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the handoff eval separate AtticDB's effect from each harness's built-in memory, and detect leaks across runs.

**Architecture:** 2 new opt-in arms and 1 relaxed CLI rule in `research/handoff_eval`.
`blank` runs only the reader, with no handoff at all. It gives the chance floor and flags any evidence leak.
`atticdb-only` is the atticdb arm with each harness's own memory switched off.
Same-harness directions (`claude-code:claude-code`, `copilot:copilot`) keep the harness's memory between writer and reader, so built-in memory gets its home case.

**Tech Stack:** Python 3 stdlib, pytest with the stub agents in `research/handoff_eval/tests/stub_agent.py`.

**Spec:** `docs/research/2026-11-03-memory-value.md`. Prior results: `research/handoff_eval/results/2026-11-04-pilot.md` and `2026-11-04-rerun.md`.

## Global Constraints

- Stdlib only. No model in any test or grader.
- Tests: `PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest research/handoff_eval/tests research/setup_eval/tests -q`. All must pass.
- Default `--arms` stays `native,files,atticdb,oracle`. New arms run only when named, so old commands cost the same.
- Memory off switches, verified 2026-11-05:
  - Claude Code: env `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` (code.claude.com/docs/en/memory).
  - Codex 0.147: `--disable memories` (same as `-c features.memories=false`; `codex features list` shows `memories ... false`).
  - Copilot CLI: memory is off in prompt mode unless `--enable-memory`. Never pass `--enable-memory`. Its memory lives in a service on the owner's real account, so eval facts would leak into real work.
- Graders in `grade.py` don't change. The evidence tokens stay fixed.
- Writing: no em dashes, sentences under 20 words. Commits use `git commit -s`, no Co-authored-by.
- Codex is out of quota until Nov 29. Codex paths get unit tests only.

## Review Focus

1. Same-harness scrub: the reader keeps the writer's native memory for this repo. Transcripts, sessions and history still go. Test: `test_same_harness_keeps_native_memory_only` (Task 3).
2. `atticdb-only` must switch memory off in the reader session too, not only the writers. Test: `test_atticdb_only_env_on_every_session` (Task 2).
3. `blank` never sets up AtticDB, never runs the hook, and never reports a capture. Test: `test_blank_plan_has_reader_only` (Task 1).
4. `--regrade` on a CSV from before the `leaked` column still works, with `leaked` read as None. Test: `test_from_row_without_leaked_column` (Task 1).
5. A same-harness Copilot run keeps nothing in COPILOT_HOME (its memory is off), so it can't carry session history to the reader. Test: `test_same_harness_copilot_keeps_nothing` (Task 3).

---

### Task 1: `blank` control arm and the `leaked` flag

**Files:**
- Modify: `research/handoff_eval/arms.py`, `research/handoff_eval/run.py`, `research/handoff_eval/results.py`, `research/handoff_eval/cli.py`, `research/handoff_eval/README.md`
- Test: `research/handoff_eval/tests/test_run.py` (arms, CLI and run tests live here) and `tests/test_results.py`

**Interfaces:**
- Produces: `arms.ARMS` ends with `"blank"`; `arms.DEFAULT_ARMS = ("native", "files", "atticdb", "oracle")`; `arms.has_writer("blank") is False`; `arms.reader_prompt(case, "blank") == case.reader_prompt`.
- Produces: `HandoffResult.leaked: bool | None = None`, set only for `blank`. CSV column `leaked` after `delivered_where`.

- [ ] **Step 1: Write the failing tests**
  - `test_blank_plan_has_reader_only`: `plan(cfg, CASES["H1"], "blank", "claude-code", "copilot", root)` has empty `setup`, `writer`, `capture` and `hook`, and 1 `reader` step whose prompt equals `CASES["H1"].reader_prompt`.
  - `test_blank_leak_flags_token_in_reply`: a stub reader that replies "LEDG-7731" in a blank run gives `leaked is True`. A reply without tokens gives `leaked is False`. Any non-blank arm gives `leaked is None`.
  - `test_blank_leak_flags_delivery`: a blank run whose reader transcript holds a token gives `leaked is True`, even when the reply doesn't cite it.
  - `test_from_row_without_leaked_column`: `results.from_row` on a dict with no `leaked` key returns `leaked is None`.
  - `test_default_arms_unchanged`: `cli` parsed with no `--arms` gives `["native", "files", "atticdb", "oracle"]`.
  - `test_summary_has_control_section`: `results.summary` on blank rows prints a "Control (blank arm)" section with pass rate and "Leaked: N of M".
- [ ] **Step 2: Run the tests, expect failures** (the arm doesn't exist yet).
- [ ] **Step 3: Implement.** `leaked = bool(tokens_in(reply, case.evidence)) or bool(delivered)` for `blank`. The CLI default uses `DEFAULT_ARMS`; `--arms` accepts all of `ARMS`. `results.row`, `from_row` and the header get `leaked`. The summary adds the control section only when blank rows exist. The README's arm list adds `blank`, with 1 line on why: chance floor and leak detector.
- [ ] **Step 4: Run the full test command, expect all pass.**
- [ ] **Step 5: Commit** `research(handoff-eval): blank control arm and leaked flag`.

### Task 2: `atticdb-only` arm, with harness memory off

**Files:**
- Modify: `research/handoff_eval/arms.py`, `research/handoff_eval/run.py`, `research/handoff_eval/README.md`
- Test: `research/handoff_eval/tests/test_run.py`

**Interfaces:**
- Consumes: `arms.ARMS` and `DEFAULT_ARMS` from Task 1.
- Produces: `"atticdb-only"` in `ARMS`, before `"blank"`. `arms.uses_atticdb(arm)` is True for `atticdb` and `atticdb-only`. `arms.native_memory_off(arm) -> bool`, True only for `atticdb-only`. `arms.session_env(harness: str, arm: str) -> dict[str, str]`, which returns `{"CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1"}` for claude-code in `atticdb-only` and `{}` otherwise. `arms.session_argv` adds `--disable memories` for codex in `atticdb-only`, right before `-m <model>`.

- [ ] **Step 1: Write the failing tests**
  - `test_atticdb_only_env_on_every_session`: in `plan(...)` for `atticdb-only` with claude-code as writer and reader, every `writer` and `reader` step has `extra_env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] == "1"`. For `atticdb`, none has it.
  - `test_atticdb_only_codex_argv`: `session_argv("codex", "p", "m", "atticdb-only")` ends with `["--disable", "memories", "-m", "m", "p"]` and keeps the AtticDB approval override.
  - `test_copilot_never_enables_memory`: for every arm, `"--enable-memory" not in session_argv("copilot", "p", "m", arm)`.
  - `test_atticdb_only_sets_up_atticdb`: the `atticdb-only` plan has setup, hook and teardown steps, the same as `atticdb`.
  - `test_atticdb_only_flags_native_memory_write`: when a stub writer still writes the token into the Claude memory dir, the result's reasons include "native memory written with memory off".
- [ ] **Step 2: Run the tests, expect failures.**
- [ ] **Step 3: Implement.** `plan()` passes `arms.session_env(...)` as `extra_env` on writer and reader steps. `run_handoff` appends the reason when `native_memory_off(arm)` and the capture scan finds a `claude-memory` or `codex-memory` hit. Replace `arm == "atticdb"` checks with `uses_atticdb(arm)` wherever they mean "AtticDB is wired". The README adds the arm and the 3 switches.
- [ ] **Step 4: Run the full test command, expect all pass.**
- [ ] **Step 5: Commit** `research(handoff-eval): atticdb-only arm with harness memory off`.

### Task 3: same-harness directions

**Files:**
- Modify: `research/handoff_eval/cli.py`, `research/handoff_eval/run.py`, `research/handoff_eval/README.md`
- Test: `research/handoff_eval/tests/test_run.py`. Its test near line 506 asserts same-harness directions are refused. Replace it with the acceptance test below.

**Interfaces:**
- Consumes: `native_memory_dir(hroot, harness, repo)` (run.py) and the arms from Tasks 1 and 2.
- Produces: `cli.direction_list("claude-code:claude-code")` returns `[("claude-code", "claude-code")]`. `run.scrub_harness_dir(hroot: str, harness: str, keep: str | None = None) -> list[str]`, where `keep` is an absolute path inside the harness dir that survives with its parents. In `run_handoff`, delivery gains `("native-memory", token)` for tokens found in the kept memory dir before the reader starts.

- [ ] **Step 1: Write the failing tests**
  - `test_same_harness_direction_accepted`: `direction_list("claude-code:claude-code,copilot:copilot")` returns both pairs. A bad harness name still raises.
  - `test_same_harness_keeps_native_memory_only`: a claude-code to claude-code run where the stub writer writes `projects/<slug>/memory/MEMORY.md` with "LEDG-7731" and a transcript `projects/<slug>/s.jsonl`. After the scrub, MEMORY.md exists and s.jsonl doesn't. Delivery includes `native-memory[LEDG-7731]`.
  - `test_cross_harness_scrub_unchanged`: claude-code to copilot still deletes the whole writer dir but the login file.
  - `test_same_harness_copilot_keeps_nothing`: copilot to copilot leaves COPILOT_HOME empty before the reader.
  - `test_atticdb_only_same_harness_keeps_nothing_in_memory`: with memory off, nothing is kept, because the memory dir doesn't exist.
- [ ] **Step 2: Run the tests, expect failures.**
- [ ] **Step 3: Implement.** Drop the same-harness refusal in `direction_list`, and update `CROSS` and the help text to list same-harness pairs. In `run_handoff`, when `writer == reader`, call `scrub_harness_dir(hroot, writer, keep=native_memory_dir(hroot, writer, repo))`. For Copilot, `native_memory_dir` is None, so nothing is kept. Record the native-memory delivery before the reader session. The README explains same-harness runs and that Copilot's native memory stays off on purpose.
- [ ] **Step 4: Run the full test command, expect all pass.**
- [ ] **Step 5: Commit** `research(handoff-eval): same-harness directions keep native memory`.

### Task 4: controlled run and audited results (controller, not a subagent)

- [ ] Build atticdb from main. Batch 1, Copilot-heavy first, because Claude's session limit is shared with Claude Desktop:
  - `--arms blank --cases H1,H2,H3,H4,H5,H6 --directions claude-code:copilot,copilot:claude-code --reps 1` (12 reader sessions).
  - `--arms native,atticdb,atticdb-only --directions copilot:copilot --reps 1` (18 runs).
- [ ] Batch 2, after a Claude probe: `--arms native,atticdb,atticdb-only --directions claude-code:claude-code --reps 1` (18 runs), then `--arms atticdb-only --directions claude-code:copilot,copilot:claude-code --reps 1` (12 runs).
- [ ] Stop rule: if any `blank` row has `leaked` True, stop. Report it, and plan per-run token rotation before citing any number.
- [ ] Hand-audit every row, pass and fail, from the full `reply` column. Write `research/handoff_eval/results/2026-11-05-controls.md`:
  - chance floor per case (blank)
  - AtticDB with vs without native memory (atticdb vs atticdb-only)
  - same-harness native vs atticdb
  - caveats: 1 rep, Copilot native memory off on purpose, Codex out
- [ ] Commit the results and CSVs, then open the PR.

## What this plan does not do

- No per-run token rotation. The blank arm detects the leak that rotation would prevent. Build rotation only if blank trips.
- No Copilot native memory. That needs a separate eval GitHub account.
- No new graders and no semantic judge.
