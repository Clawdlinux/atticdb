# handoff_eval

Does a coding decision survive a handoff from Claude Code to Codex, and back?
Harness A gets the writer prompts. Harness B gets the reader prompt, in the same repo.
We measure 3 stages per run: captured, delivered and passed.

The first pilot is an integration pilot. It is not a statistical result.

Python 3, standard library only. Tests use pytest and stub agents. They never start the real `claude` or `codex`.

## Isolation

Every rule in `research/setup_eval/README.md` "Isolation" holds unchanged.
This package reuses setup_eval's guard, env, process runner and cleanup. It doesn't fork them.

- The eval config dirs stay `~/.cache/atticdb-setup-eval/{claude,codex}`. The guard refuses any that is, or lies in, `~/.claude*`, `~/.codex` or `~/.copilot`.
- Each run gets a new temp root. It has its own AtticDB home, `<root>/zh`, and its own fixture repo, `<root>/evalrepo`.
- Every child gets setup_eval's env: no inherited `ATTICDB_*`, `GIT_*`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or credential vars. A child whose `ATTICDB_HOME` is not under its run root is refused.
- Before and after a batch, the real AtticDB home and the top level of `~/.claude*`, `~/.codex` and `~/.copilot` are compared by stat only. A change prints a warning.

Each run starts clean, and checks that it did.
Before any agent starts, it searches the store for each evidence token.
It also scans the repo and Claude's memory dir for this repo path.
Any hit makes the run `errored` in stage `start`, and no agent runs.

The Codex memories dir, `<CODEX_HOME>/memories/`, is shared by every run.
A file there that already holds a token is named in the reasons as stale memory.
It doesn't count as capture. It could still reach a Codex reader, so look for it in the reasons.

After each run: `atticdb disconnect` for both harnesses, stop the AtticDB server the run started, delete the root.
Each run also disconnects both harnesses before it starts. So wiring left by a crashed run can't leak into a native or files arm.

## Arms

| Arm | What the writer and reader get |
|---|---|
| native | The repo files only. |
| files | AGENTS.md and CLAUDE.md in the fixture say: "Keep HANDOFF.md at the repo root. After each task, record decisions, their reasons, rejected approaches and constraints. Read it before you start." Nothing is pre-filled. The writer has to keep the file. |
| atticdb | `atticdb setup --yes --harness <h>` for both harnesses before the writer. Both are disconnected after the reader. |
| oracle | No writer session. The reader prompt starts with "Notes from the last session: " and the case's gold note. |

In the files arm, AGENTS.md and CLAUDE.md are committed with the fixture. So they are not part of any diff.

Each writer prompt is its own headless session. H3 has 2. Writers run in harness A's eval config dir, the reader in harness B's.
The commands come from `setup_eval.agents.agent_argv`, with the model flag added:

- `claude`: `--model <id>`, last.
- `codex exec`: `-m <id>`, just before the prompt. The prompt stays the last argument.

Outside the atticdb arm, Codex runs without the `-c mcp_servers.atticdb.tools.fact_set.approval_mode=...` override.
With no atticdb server configured, that override alone makes a partial server table.
Codex 0.147 then refuses to load its config: "invalid transport in `mcp_servers.atticdb`".

## Stages

Each stage is 1, 0, or empty. Empty means not applicable, or not measured because the run errored first.

**captured**, after the writer sessions. True when any of the case's evidence tokens is found in 1 of 3 places:

1. The run's AtticDB store: `atticdb search --query <token>`, matched on each hit's content and key.
2. Any file in the repo working tree, tracked or not. `.git` is skipped.
3. The writer harness's native memory for this repo, and only files changed during the run:
   - Claude: `<CLAUDE_CONFIG_DIR>/projects/<slug>/memory/`. The slug is the repo's real path with every character that is not a letter or digit turned into `-`. That is how Claude names it. `/private/var/x_y/evalrepo` gives `-private-var-x-y-evalrepo`.
   - Codex: `<CODEX_HOME>/memories/`.

`captured_where` lists each place with the tokens found there, such as `atticdb:fact repo/evalrepo/x[LEDG-7731]; repo:HANDOFF.md[LEDG-7731,412]`.
The oracle has no capture.

**delivered**, before the reader's final reply. True when a token was in its context:

- `hook`: the block that `atticdb hook session-start --harness <reader>` prints just before the reader starts. atticdb arm only.
- `transcript`: a tool result or file read in the reader's raw stream, before its final assistant message. Claude: `tool_result` blocks and `tool_use_result`. Codex: command output and MCP results. The commands the agent typed don't count.
- `prompt`: the oracle's reader prompt.

H5 has no delivery stage. Its evidence is H1's tokens, which are unrelated history there.

**passed**: `grade.grade` on the reader's final reply, `git diff refs/eval/base`, and the repo it leaves.
A failed code check that timed out gets "check timed out" in the reasons.

**errored**: a session that timed out, did not start, exited non-zero, reported an error, or gave no final reply.
`error_stage` says where: `fixture`, `setup`, `start`, `writer`, `capture`, `hook`, `reader`, `grade`, `budget` or `teardown`.
An errored run is kept in the CSV. It's never a pass or a fail, and the batch goes on.

## Cost

A run is its writer sessions plus 1 reader. The oracle has no writer.
The pilot matrix is 6 cases, 4 arms, 2 directions and 1 rep: 48 runs and 90 agent sessions. 42 are writers and 48 are readers.
Each session has a 300 s timeout by default (`--agent-timeout`).

`--max-sessions` is required for a real run. It counts writer and reader sessions both.
A batch over it is refused before anything starts.
While a batch runs, it stops before a run that would pass it. A session past it never starts.

## Run

Log in once, as in setup_eval:

```sh
CLAUDE_CONFIG_DIR=~/.cache/atticdb-setup-eval/claude claude auth login
CODEX_HOME=~/.cache/atticdb-setup-eval/codex codex login
```

Then:

```sh
cargo build -p atticdb
python3 -m research.handoff_eval --check
python3 -m research.handoff_eval --dry-run --cases H1,H2,H3,H4,H5,H6 --arms native,files,atticdb,oracle \
    --directions claude-code:codex,codex:claude-code --reps 1 --claude-model <id> --codex-model <id>
python3 -m research.handoff_eval --cases H1,H2,H3,H4,H5,H6 --arms native,files,atticdb,oracle \
    --directions claude-code:codex,codex:claude-code --reps 1 --claude-model <id> --codex-model <id> \
    --max-sessions 90 --out research/handoff_eval/results/2026-11-04-pilot.csv
PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest research/handoff_eval/tests research/setup_eval/tests -q
```

`--check` is setup_eval's check. It runs `claude auth status` and `codex login status` on the eval dirs, and prints only whether each is logged in.
`--dry-run` prints every planned session, with its command, cwd, timeout and env names, then the total. It starts nothing and writes nothing.

| Flag | Default | Meaning |
|---|---|---|
| `--cases` | all 6 | a comma list of H1 to H6 |
| `--arms` | all 4 | a comma list of native, files, atticdb, oracle |
| `--directions` | both | a comma list of `writer:reader` |
| `--reps` | 1 | runs per case, arm and direction |
| `--claude-model`, `--codex-model` | none | required for each harness the directions use |
| `--max-sessions` | none | required for a real run |
| `--atticdb` | `target/debug/atticdb` | the AtticDB binary |
| `--out` | `research/handoff_eval/results/<date>.csv` | rows are appended. The summary goes to the same path with `.md`, for this batch only |
| `--agent-timeout` | 300 | seconds per agent session |
| `--keep-api-keys` | off | pass the credential and provider vars on |

Exit codes: 0 when every run finished, 1 when a run errored or the budget stopped the batch, 2 for a refusal or a bad flag.

## Results

The CSV has 1 row per run: case, arm, writer, reader, rep, the 3 stages and where each was found, errored and its stage, sessions, tokens in and out, wall time, both model ids, and the reasons.
Reasons hold the grader's reasons and a redacted excerpt of the reply. Never a full reply or an env value.

The summary's first line is "Integration pilot. Not a statistical result."
Then pass rates by arm and case, a stage funnel per arm with the oracle ceiling, and pass rates by direction.
Rates count runs that did not error. Errored runs are counted apart.
