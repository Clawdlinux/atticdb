# handoff_eval

Does a coding decision survive a handoff from Claude Code to Codex, and back?
Harness A gets the writer prompts. Harness B gets the reader prompt, in the same repo.
We measure 3 stages per run: captured, delivered and passed.

The first pilot is an integration pilot. It is not a statistical result.

Python 3, standard library only. Tests use pytest and stub agents. They never start the real `claude` or `codex`.

## Isolation

Every rule in `research/setup_eval/README.md` "Isolation" holds unchanged.
This package reuses setup_eval's guard, env, process runner and cleanup. It doesn't fork them.

- The shared eval config dirs stay `~/.cache/atticdb-setup-eval/{claude,codex}`. The guard refuses any that is, or lies in, `~/.claude*`, `~/.codex` or `~/.copilot`.
- Each run gets a new temp root. It has its own AtticDB home, `<root>/zh`, and its own fixture repo, `<root>/evalrepo`.
- Each run also gets its own config dirs. The guard checks them too:
  - `CODEX_HOME` is `<root>/codex`. It gets copies of the eval dir's `auth.json` and `config.toml`, nothing else. So Codex sessions, memories and history never carry over. Without `auth.json` the run errors at `setup` with the login hint.
  - `CLAUDE_CONFIG_DIR` is `<root>/claude`, and starts empty.
- Every child gets setup_eval's env: no inherited `ATTICDB_*`, `GIT_*`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or credential vars. A child whose `ATTICDB_HOME` is not under its run root is refused.
- Before and after a batch, the real AtticDB home and the top level of `~/.claude*`, `~/.codex` and `~/.copilot` are compared by stat only. A change prints a warning.

### How Claude logs in from an empty dir

On macOS, Claude keeps its claude.ai login in the Keychain, not in the config dir.
The Keychain item's name ends with the first 8 hex chars of the SHA-256 of the config dir.
So a copied dir isn't logged in.
Claude 2.1.310 reads `CLAUDE_SECURESTORAGE_CONFIG_DIR` to pick that name instead.
Every child gets it set to the eval Claude dir, replacing any inherited value.
So a run finds the eval login, and shares nothing else with other runs.
`--check` tests this: it logs in from a run's own dirs, as a run would.

Codex may refresh its login during a run.
If the run's `auth.json` changed and is a JSON object, it replaces the eval dir's, atomically, with mode 600.
Otherwise a rotated refresh token would be lost with the root.

### Clean start

Each run starts clean, and checks that it did:

1. Both harnesses are disconnected from AtticDB. Outside the atticdb arm, a failed disconnect errors the run at `setup`.
2. No config the run will use may mention atticdb: `settings.json` and `.claude.json` in the Claude dir, `config.toml`, `hooks.json` and `AGENTS.md` in the Codex dir. Codex's `[projects."<path>"]` trust headers are skipped. A hit errors the run at `setup`.
3. The store is searched for each evidence token. The repo and both of the run's config dirs are scanned too, except `auth.json`. A hit errors the run at `start`.

No agent runs in a run that errored there.

After each run: `atticdb disconnect` for both harnesses, stop the AtticDB server the run started, delete the root with its config dirs.

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

Every match goes through `EVIDENCE_PATTERNS` in run.py, 1 pattern per token. Capture, delivery and the clean start all use it:

| Token | Counts as |
|---|---|
| `LEDG-7731`, `INC-2209`, `STOR-5150`, `bramble-ci`, `FIN-88` | the whole token, any case, with no letter or digit next to it |
| `412` | `412 bytes` or `412-byte`. A bare 412, such as a file size of 4120, doesn't count |
| `3.9` | `Python 3.9`, or 3.9 within 40 chars of `bramble-ci`. `Python 3.9.6` from a version check doesn't count |
| `plain files` | `plain file`, `plain-file` or `plain files` |

**captured**, after the writer sessions. True when any of the case's evidence tokens is found in 1 of 3 places:

1. The run's AtticDB store: `atticdb search --query <token>`, matched on each hit's content and key.
2. Any file in the repo working tree, tracked or not. It skips `.git`, `.venv`, `venv`, `__pycache__`, `.pytest_cache`, `node_modules` and any dir holding `pyvenv.cfg`.
3. The writer harness's native memory for this repo, in the run's own config dir. Only files changed during the run count:
   - Claude: `<CLAUDE_CONFIG_DIR>/projects/<slug>/memory/`. The slug is the repo's real path with every character that is not a letter or digit turned into `-`. That is how Claude names it. `/private/var/x_y/evalrepo` gives `-private-var-x-y-evalrepo`.
   - Codex: `<CODEX_HOME>/memories/`.

`captured_where` lists each place with the tokens found there, such as `atticdb:fact repo/evalrepo/x[LEDG-7731]; repo:HANDOFF.md[LEDG-7731,412]`.
The oracle has no capture.

**delivered**, before the reader's final reply. True when a token was in its context:

- `hook`: the block that `atticdb hook session-start --harness <reader>` prints just before the reader starts. atticdb arm only.
- `transcript`: a tool result or file read in the reader's raw stream, before its final assistant message. Claude: `tool_result` content, and a Read's `tool_use_result.file.content`. Write and Edit results are left out, as they echo what the reader wrote. Codex: command output and MCP results. The commands the agent typed don't count.
- `prompt`: the oracle's reader prompt.

H5 has no delivery stage. Its evidence is H1's tokens, which are unrelated history there.

**passed**: `grade.grade` on the reader's final reply, `git diff refs/eval/base`, and the repo it leaves.
`check_timed_out` is 1 when a code check timed out. grade.py has no flag of its own yet, so its reasons are read for "timed out". A fail with it also gets "check timed out" in the reasons.

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
Then it runs both again from a run's own config dirs.
`--dry-run` prints every planned session, with its command, cwd, timeout and env names, then the total. It starts nothing and writes nothing.

| Flag | Default | Meaning |
|---|---|---|
| `--cases` | all 6 | a comma list of H1 to H6 |
| `--arms` | all 4 | a comma list of native, files, atticdb, oracle |
| `--directions` | both | a comma list of `writer:reader`. Same-harness directions are refused, as the pilot is cross-harness only |
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
