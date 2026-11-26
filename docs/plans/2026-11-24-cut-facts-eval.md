# Cut facts eval Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure whether an agent reads a `(cut)` fact in full before it edits code that the cut part governs.

**Architecture:** A new package, `research/cut_fact`, runs 1 agent session per cell against a real AtticDB home with the real hook and MCP server. It reuses `handoff_eval` and `setup_eval` isolation as is. Grading is mechanical: the transcript's tool calls, and the lines added under `app/`.

**Tech Stack:** Python 3 stdlib, pytest with stub agents, `claude -p` first. Copilot CLI second. Codex waits for quota (#190).

**Spec:** [#234](https://github.com/Clawdlinux/atticdb/issues/234).

**As built (2026-11-26):** the cases went into `research/setup_eval` as opt-in scenarios, not a new `research/cut_fact` package. `setup_eval` already runs 1 harness per run with a real hook and MCP server. It needed 1 new field, the ordered tool list. Results: `research/setup_eval/results/2026-11-26-cutfact.md`.

## Global Constraints

- The hook cuts a fact's content at `MAX_CONTENT` = 300 chars and appends ` (cut)` (`crates/atticdb/src/hook.rs:35`, `:438`).
- No hook change in this plan. #234 says: report failures before changing the hook.
- Reuse, don't fork: `handoff_eval.run.make_harness_root`, `prepare_config_dirs`, `setup_env`, `env_for`, `tool_outputs`, and `setup_eval`'s guard and runner.
- Tests never start a real agent. Raw transcripts go to `~/.cache/atticdb-cut-fact/<batch>/`, never into the repo.
- Every run has a hard timeout. Default 600 s per session, as in `stale_rule`.
- Writing rules from `CONTRIBUTING.md`. Commits use `git commit -s`.

## Review Focus

- A fact whose exception sits at char 280 to 320 may straddle the cut. The fixture must put the whole exception past char 320, and a test must pin that.
- The `full` control can't change the hook. It stores the same content reordered so the exception is inside the first 300 chars. A test must pin that both texts hold the same sentences.
- An agent may read the fact through `memory_search`, not `fact_get`. Both count as a read if the reply holds the exception token.
- "Edited before reading" needs a strict order. Use the first `Edit`, `Write` or `MultiEdit` tool call index, not wall time.
- A run whose MCP server never connected is an error, not a fail. Check the init event lists `atticdb` as connected.

---

### Task 1: Fixtures

**Files:**
- Create: `research/cut_fact/__init__.py`, `research/cut_fact/fixtures.py`
- Test: `research/cut_fact/tests/test_fixtures.py`

**Interfaces:**
- Produces: `CASES: dict[str, Case]`, with `Case(name, key, rule, exception, token, task, files)`. `token` is a unique word in `exception` only, such as `LEDG-7731`.
- Produces: `fact_text(case, arm) -> str | None`. Arms `cut`, `full`, `absent`. `cut` puts `exception` after a rule body of 320+ chars. `full` puts `exception` first. `absent` returns `None`.
- Produces: `build(case, repo) -> None`. A tiny git repo with passing tests, where the task touches the code the exception governs.

At least 3 cases. Each exception makes the naive edit wrong but still pass the repo's tests, so a wrong choice is silent, as in `stale_rule`.

- [ ] **Step 1:** Write tests: the `cut` text's `token` index is >= 320, the `full` text's is < 300, both hold the same sentences, `build` passes its own tests.
- [ ] **Step 2:** Run `PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest -q research/cut_fact/tests`. Expected: FAIL, module missing.
- [ ] **Step 3:** Implement `fixtures.py`.
- [ ] **Step 4:** Rerun. Expected: PASS.
- [ ] **Step 5:** Commit: `research(cut_fact): fixtures for #234`.

### Task 2: Runner and grader

**Files:**
- Create: `research/cut_fact/run.py`, `research/cut_fact/grade.py`, `research/cut_fact/__main__.py`, `research/cut_fact/README.md`
- Test: `research/cut_fact/tests/test_grade.py`, `research/cut_fact/tests/test_run.py`

**Interfaces:**
- Consumes: Task 1's `CASES`, `fact_text`, `build`. `handoff_eval.run` helpers named in Global Constraints.
- Produces: `grade(events: list[dict], diff: str, case: Case) -> dict` with keys `read` (bool), `read_before_edit` (bool), `verdict` (`exception`, `naive`, `mixed`, `none`), `correct` (bool: verdict is `exception`).
- Produces: CLI `python3 -m research.cut_fact run --batch <b> --harness claude --arms cut full absent --reps <n> --jobs <n> --model <id> --out <csv>` and `summary <csv>`. CSV columns mirror `stale_rule`.

Per run: new root and harness root, `atticdb setup --yes --harness <h>` into the run's own home, `atticdb fact set` the case's fact under `repo/evalrepo/<area>/<topic>` unless `absent`, then 1 session with the task prompt. The prompt never mentions memory.

- [ ] **Step 1:** Write grader tests from stub transcripts: read then edit, edit then read, `memory_search` read with the token, no read, MCP not connected (error row).
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement `grade.py`, then `run.py` with a stub runner test that checks the env never names a real `~/.claude*`, `~/.codex` or `~/.copilot`.
- [ ] **Step 4:** Run the package suite. Expected: PASS.
- [ ] **Step 5:** Add `research/cut_fact/tests` to the research step in `.github/workflows/ci.yml`. Commit: `research(cut_fact): runner and grader for #234`.

### Task 3: Pilot on Claude Code

- [ ] **Step 1:** `--check` first, then 1 cell: `timeout 900 python3 -m research.cut_fact run --batch 2026-11-25-smoke --harness claude --arms cut --reps 1 --jobs 1 --out /tmp/cut-smoke.csv`. Expected: 1 graded row, MCP connected.
- [ ] **Step 2:** Full pilot: 3 cases x 3 arms x 3 reps = 27 runs, `--jobs 4`, under `timeout 7200`. Expected cost about $3, from `stale_rule`'s cost per run.
- [ ] **Step 3:** Write `research/cut_fact/results/<date>-results.md`: correct by arm, read rate, read-before-edit rate, limits. Commit with the CSV.

### Task 4: Report

- [ ] Post the table on #234. If `cut` scores below `full`, open 1 follow-up issue with the failing transcripts' pattern. Don't change the hook in this PR.
- [ ] Copilot CLI cells after the Claude pilot, same commands with `--harness copilot`.

## What this does NOT do

- No hook or MCP change.
- No Codex cells until its quota returns (#190).
- No claim about real repos or other models. 3 synthetic cases, 1 model.
