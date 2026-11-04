# Cross-harness handoff eval: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure whether AtticDB helps a coding decision survive a Claude Code to Codex handoff (and back) better than plain repo files or a maintained HANDOFF.md, and find which stage fails: capture, delivery or use.

**Architecture:** A new stdlib-only Python package, `research/handoff_eval`, reuses `research/setup_eval`'s isolation, process runner, transcript parsers and AtticDB setup steps. Each run: a fresh fixture repo and AtticDB home, 1 or 2 writer sessions in harness A, then 1 reader session in harness B, under one of 4 arms. Deterministic graders check facts and code, with counterexample tests so a keyword can't pass a wrong answer. Results are a CSV plus a stage funnel per arm.

**Tech Stack:** Python 3 stdlib, pytest for tests, `claude` and `codex` CLIs, the `atticdb` binary.

**Spec:** `docs/research/2026-11-03-memory-value.md` (sections "Provisional Benchmark Contract" and "Bottleneck Evidence"). Read it first.

## Global Constraints

- Stdlib only for the package. pytest only for tests. No new dependencies (CONTRIBUTING.md).
- Every isolation rule in `research/setup_eval/README.md` "Isolation" holds unchanged. Never read or write the real `~/.claude*`, `~/.codex`, `~/.copilot` or the real AtticDB home. The eval config dirs stay `~/.cache/atticdb-setup-eval/{claude,codex}`.
- Each run gets a new temp root: its own AtticDB home and its own fixture repo. Nothing carries over between runs.
- Separate the 3 questions in the spec: storage, memory and enterprise learning. This plan is memory only. No single "accuracy" score.
- Model IDs are explicit flags (`--claude-model`, `--codex-model`), recorded in every row. No silent default.
- A run that times out or errors is recorded as `errored`, never dropped and never counted as a pass or fail.
- A sub-budget pilot is an integration pilot, not statistical proof. Results files say so in their first line.
- Writing: `/Users/sunny/clawdlinux/WRITING_STYLE.md`. No em dashes. Sentences under 20 words.
- Commits: `git commit -s`. No Co-authored-by trailer.

## Review Focus

1. A correct answer in other words must pass, and a wrong answer that reuses the right keywords must fail. Each case ships a table of both kinds (Task 2).
2. Prompt leak: no reader prompt, fixture file or setup text may contain a case's evidence tokens. Otherwise "delivery" is free (Task 2 test `test_no_reader_prompt_leaks_evidence`).
3. Cross-run leakage: a store or repo left over from a previous run would fake capture. Each run asserts its AtticDB store and repo start empty of all evidence tokens (Task 3 test `test_run_starts_clean`).
4. A harness timeout or crash mid-handoff must yield `errored` with the stage it died in, and the batch keeps going (Task 3 test `test_writer_timeout_is_errored_not_failed`).
5. A budget overrun: the batch must stop before it starts a session past `--max-sessions`, counting writer and reader sessions both (Task 3 test `test_budget_counts_every_session`).

---

### Task 1: S2 scorer requires the reason, not a keyword

The spec reproduced a false positive. "We dropped Redis because its license forbids reporting p99 metrics." passes S2, because S2 accepts any of `round trip`, `p99`. Fix that first, since `setup_eval` results are cited elsewhere.

**Files:**
- Modify: `research/setup_eval/scenarios.py` (`REDIS_MARKERS`, `check_search`)
- Test: `research/setup_eval/tests/test_rules.py`

**Interfaces:**
- Produces: `check_search(out: Outcome) -> Verdict`, same signature. A pass now needs a `memory_search` call AND a round-trip mention AND a budget mention (`p99` or `1 ms`/`1ms`). A reply that blames something else (license, cost, memory, crash) fails, even when it says p99.

- [ ] **Step 1: Write the failing test** `test_search_needs_the_real_reason` as a table:
  - Must pass: "Its 2 extra round trips per append broke our 1 ms p99 budget." and "Redis added two round-trips on every append, so we missed the p99 target of 1ms."
  - Must fail:
    - "We dropped Redis because its license forbids reporting p99 metrics."
    - "Redis cost too much at p99."
    - "Round trips." (no budget)
    - "p99" (no reason)
  - Each row has a `memory_search` call.
- [ ] **Step 2:** `python3 -m pytest research/setup_eval/tests/test_rules.py -q`. Expected: the new test FAILS on the license row.
- [ ] **Step 3:** Replace the any-marker check with 2 required groups: round trip (`round trip`, `round-trip`, `roundtrip`) and budget (`p99`, `1 ms`, `1ms`). Both must appear. Update the README's S2 line to match.
- [ ] **Step 4:** Run the whole `research/setup_eval/tests` suite. Expected: PASS.
- [ ] **Step 5:** Commit: `fix(setup_eval): S2 needs the round-trip reason and the budget, not either keyword`.

### Task 2: Handoff cases and graders, no agents

**Files:**
- Create: `research/handoff_eval/__init__.py`, `research/handoff_eval/cases.py`, `research/handoff_eval/grade.py`, `research/handoff_eval/fixture.py`
- Test: `research/handoff_eval/tests/test_cases.py`, `research/handoff_eval/tests/test_grade.py`

**Interfaces:**
- Produces in `cases.py`:
  - `@dataclass(frozen=True) class Case: id: str; kind: str; writer_prompts: tuple[str, ...]; reader_prompt: str; gold_note: str; evidence: tuple[str, ...]`
    - `kind` is one of `"answer"` or `"code"`.
    - `evidence` holds unique tokens that prove the fact moved. They appear in the writer prompts and the gold note, never in the reader prompt or the fixture.
  - `CASES: dict[str, Case]` with exactly the 6 below.
- Produces in `grade.py`: `grade(case: Case, reply: str, diff: str, repo: str) -> Verdict` (reuse `setup_eval.scenarios.Verdict`). For `code` cases it runs the case's check against the repo with `python3`, timeout 20 s, and no network.
- Produces in `fixture.py`: `write_fixture(repo: str) -> None` creates the fixture repo files and 1 commit, using `setup_eval.agents.git_env()`. Files:
  - `ledger.py`: `pack_header(n)` using JSON, `export(rows)` sequential, `parse(line)`.
  - `test_ledger.py`: passing stdlib unittest.
  - README.md: one line, no hints.

**The 6 cases.** Exact tokens are fixed here. The writer prompts are natural user messages: they state the decision as a user would, and they never mention memory, AtticDB or HANDOFF.md.

| id | kind | writer prompt(s) (harness A) | reader prompt (harness B) | pass needs | fail if |
|---|---|---|---|---|---|
| H1 reason | answer | "Switch `pack_header` in ledger.py from JSON to `struct`. We measured JSON headers at 412 bytes vs 24 bytes with struct, see ticket LEDG-7731." | "Why does ledger.py pack headers with struct instead of JSON? 1 or 2 lines." | `LEDG-7731`, or both `412` and `24` | gives a reason with neither |
| H2 rejected | code | "Keep `export` in ledger.py sequential. Never use threads or a pool there: ThreadPoolExecutor deadlocked on the shared SQLite handle in incident INC-2209. Add a docstring to export, nothing else." | "export() in ledger.py is too slow on big inputs. Make it faster." | tests pass, and no added line imports `threading`, `concurrent`, `multiprocessing` or `asyncio` | any such import |
| H3 changed | answer | 1: the H2 writer prompt. 2: "We removed the SQLite handle from ledger.py today. Storage is plain files now, so the INC-2209 deadlock can't happen anymore." | "Can export() in ledger.py use a thread pool now? Answer yes or no first, then why." | starts with yes, and cites INC-2209 or the SQLite removal | starts with no |
| H4 paraphrase | answer | "Heads up: our CI runner `bramble-ci` only has Python 3.9. So no match statements anywhere in this repo." | "I want to use structural pattern matching in `parse` in ledger.py. Is that OK in this repo? Yes or no first." | starts with no, and cites 3.9 or `bramble-ci` | starts with yes |
| H5 abstain | answer | the H1 writer prompt (unrelated history, so the store isn't empty) | "What retention period did we pick for exported ledger files? 1 line." | says it found no record (marker list in grade.py) | states any number with a time unit |
| H6 constraint | code | "Rule for ledger.py: money is always integer cents, never float. Bug FIN-88 lost a cent per 10k rows from float rounding." | "Add `total(rows)` to ledger.py. It sums the `amount` field of each row dict." | `total([{"amount":1},{"amount":2}]) == 3` and is an int; `float` doesn't appear in total's source; tests pass | float anywhere in total |

`gold_note` is 1 or 2 sentences per case, carrying the same facts as the writer prompts. H5's gold note says nothing about retention. H5's `evidence` is H1's tokens: its capture stage checks that the store isn't empty. Its delivery stage doesn't apply, so it's recorded as `None`.

Deferred from the spec's case list (ruling, recorded in the ledger): distinguishing similar projects, poisoned memory, the "native memory enabled" arm (cross-harness, the writer's native memory isn't readable by the other harness), and a third-party memory arm. Add them after the pilot if the funnel shows the basics work.

- [ ] **Step 1: Write failing tests.**
  - `test_grade_table`: per case, ≥3 must-pass and ≥3 must-fail rows. Must-fail rows include the right keywords used wrongly. Example for H4: "Yes, 3.9 on bramble-ci supports it." For H3: "No, INC-2209 means no threads."
  - `test_no_reader_prompt_leaks_evidence`: no evidence token of any case appears in any reader prompt or in any fixture file.
  - `test_code_checks_run_offline`: H2 and H6 checks, against a repo you build in the test.
- [ ] **Step 2:** `python3 -m pytest research/handoff_eval/tests -q`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement `cases.py`, `grade.py` and `fixture.py`. "Starts with yes/no" means the first word, after stripping markdown and punctuation, case-insensitive.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(handoff_eval): 6 handoff cases with graders that reject right keywords used wrongly`.

### Task 3: Arms, sessions and stage measurement

**Files:**
- Create: `research/handoff_eval/arms.py`, `research/handoff_eval/run.py`, `research/handoff_eval/results.py`, `research/handoff_eval/cli.py`, `research/handoff_eval/__main__.py`, `research/handoff_eval/README.md`
- Test: `research/handoff_eval/tests/test_run.py` (stub agents, reuse `setup_eval/tests/stub_agent.py` and `helpers.py`)

**Interfaces:**
- Consumes: `cases.CASES`, `grade.grade`, `fixture.write_fixture`. From `setup_eval`: `guard`, `proc.Runner`, `parse.parse_claude`/`parse_codex`, `agents.agent_argv`/`setup_argv`/`disconnect_argv`/`hook_argv`/`child_env`, and `harness.hook_block`/`stop_server`/`make_root`.
- Produces:
  - `ARMS = ("native", "files", "atticdb", "oracle")`
    - native: repo files only.
    - files: AGENTS.md and CLAUDE.md in the fixture say: "Keep HANDOFF.md at the repo root. After each task, record decisions, their reasons, rejected approaches and constraints. Read it before you start." The writer harness must actually maintain it. Nothing is pre-filled.
    - atticdb: `atticdb setup --yes --harness <h>` for both harnesses before the writer, and disconnect both after.
    - oracle: no writer session. The reader prompt gets the case's `gold_note`, prepended as "Notes from the last session: ...".
  - `run_handoff(cfg, runner, case: Case, arm: str, writer: str, reader: str, index: int) -> HandoffResult`
  - `@dataclass class HandoffResult`, with fields:
    - case, arm, writer, reader, run
    - `captured: bool | None`, `captured_where: str`
    - `delivered: bool | None`
    - `passed: bool | None`
    - `errored: bool`, `error_stage: str`
    - reasons, sessions, tokens in/out, `wall_s`, models
    - `None` means not applicable. Oracle has no capture.
- Stage definitions:
  - **captured**: after the writer sessions, any evidence token is found in one of 3 places. 1: the run's AtticDB store, via `atticdb search --query <token>` with the run's `ATTICDB_HOME`. 2: any file in the repo working tree, tracked or not. 3: the writer harness's native memory for this project.
    - Claude: `<CLAUDE_CONFIG_DIR>/projects/<slug of repo path>/memory/`.
    - Codex: `<CODEX_HOME>/memories/`. Only files modified during this run count.
    - Record which place.
  - **delivered**: before the reader replies, an evidence token is in its context. Either the hook block from `hook_argv` for the reader harness, or a tool result or file read in the reader's raw transcript, counted only before the final assistant message.
  - **passed**: `grade.grade(...)` on the reader's final reply and repo diff.
- CLI: `python3 -m research.handoff_eval --cases H1,... --arms ... --directions claude-code:codex,codex:claude-code --reps N --claude-model ID --codex-model ID --max-sessions N --out results/<date>.csv [--dry-run] [--check]`. `--dry-run` prints every planned session and the total session count, with no agent run. `--check` reuses setup_eval's login and isolation check.

- [ ] **Step 1: Write failing tests** with stub agents:
  - `test_atticdb_arm_captures_and_delivers`: the stub writer calls `fact_set` with the token, and the reader's hook block contains it.
  - `test_files_arm_reads_handoff`: the stub writer writes HANDOFF.md, the stub reader reads it.
  - `test_native_arm_has_no_capture_path`: captured is False when the stub writes nothing.
  - `test_oracle_skips_writer`
  - `test_run_starts_clean`
  - `test_writer_timeout_is_errored_not_failed`
  - `test_budget_counts_every_session`
  - `test_models_are_required_and_recorded`
  - `test_dry_run_runs_nothing`
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement. Reuse setup_eval's per-run cleanup: disconnect, stop the server, delete the root. Writer and reader prompts go through `agents.agent_argv`. The model flag is appended:
  - Claude: `--model <id>`.
  - Codex: `-m <id>`.
  - Writer sessions run in harness A's eval config dir, the reader in harness B's.
- [ ] **Step 4:** Run all `research/handoff_eval/tests` and `research/setup_eval/tests`. Expected: PASS.
- [ ] **Step 5:** `results.py`:
  - A CSV row per HandoffResult.
  - `summary()`: a markdown table of arm × case pass rates, plus a funnel per arm: captured, delivered, passed, oracle ceiling.
  - The first line reads: "Integration pilot. Not a statistical result."
  - README documents isolation, arms, stages, cost and the exact commands.
- [ ] **Step 6:** Commit: `feat(handoff_eval): run handoffs across 4 arms and record capture, delivery and outcome`.

### Task 4: Pilot run and diagnosis (controller)

Not an implementer task. The controller runs it, because it spends the owner's model subscriptions.

- [ ] `python3 -m research.handoff_eval --check`. Then `--dry-run` for the pilot matrix: 6 cases × 4 arms × 2 directions × 1 rep. Record the session count. Expected about 80 sessions.
- [ ] Pick explicit model IDs available in each CLI today. Run the pilot with `--max-sessions` set to that count. Write `research/handoff_eval/results/2026-11-04-pilot.csv` and `...-pilot.md`, using the summary plus a short diagnosis.
- [ ] Diagnose with the spec's table "Diagnose The Failing Stage". For each failing atticdb cell, name the first false stage, and compare with the oracle and files arms.
- [ ] Commit the results. Open the PR for Tasks 1 to 4.

### Task 5: Gap plan from measured failures (controller)

The spec says: fix the stage that fails, not the one we guessed. So the gap tasks are written after Task 4, as `docs/plans/2026-11-04-memory-gaps.md`, using these rules:

- atticdb `captured` below the files arm: capture is the gap. Candidates: chat-start guidance that names decisions and rejected approaches, and a session-end capture path (#72).
- Captured but not `delivered`: the hook block or retrieval is the gap. Candidates: decision-aware block selection, search on paraphrase (H4).
- Delivered but failed, while the oracle passes: delivery format or ordering is the gap.
- H3 fails in the atticdb arm with capture OK: supersession is the gap. Candidates: fact revisions carrying the reason a decision changed, and structured `source_refs` (machine.rs:120 rejects them today).
- The oracle fails too: not a memory problem. Don't change AtticDB for that case.

Then execute that plan with subagent-driven development. Rerun the Task 4 pilot with the same pins and matrix, and report both runs side by side.
