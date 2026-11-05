# handoff_eval

Does a coding decision survive a handoff from Claude Code to Codex or Copilot CLI, and back?
Harness A gets the writer prompts. Harness B gets the reader prompt, in the same repo.
We measure 3 stages per run: captured, delivered and passed.

The first pilot is an integration pilot. It is not a statistical result.

Python 3, standard library only. Tests use pytest and stub agents. They never start the real `claude`, `codex` or `copilot`.

## Isolation

Every rule in `research/setup_eval/README.md` "Isolation" holds unchanged.
This package reuses setup_eval's guard, env, process runner and cleanup. It doesn't fork them.

- The shared eval config dirs stay `~/.cache/atticdb-setup-eval/{claude,codex}`. The guard refuses any that is, or lies in, `~/.claude*`, `~/.codex` or `~/.copilot`.
- Each run gets a new temp root. It has its own AtticDB home, `<root>/zh`, and its own fixture repo, `<root>/evalrepo`.
- Each run also gets its own config dirs, in a second temp dir, `<harness-root>`. It's made apart from the root, so it's neither a parent nor a sibling of the repo. A reader's `grep -r ..` can't reach it. The guard checks these dirs too:
  - `CODEX_HOME` is `<harness-root>/codex`. It gets copies of the eval dir's `auth.json` and `config.toml`, nothing else. So Codex sessions, memories and history never carry over. Without `auth.json` the run errors at `setup` with the login hint.
  - `CLAUDE_CONFIG_DIR` is `<harness-root>/claude`, and starts empty.
  - `COPILOT_HOME` is `<harness-root>/copilot`, and starts empty. See "Copilot CLI" below.
- The reader's env still names the writer harness's dir, and its transcripts and sessions hold the writer's prompts. So after the capture scan, that dir loses everything but its login file: `.credentials.json` for Claude, `auth.json` for Codex, nothing for Copilot.
- Every child gets setup_eval's env: no inherited `ATTICDB_*`, `GIT_*`, `COPILOT_*`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or credential vars, then the run's own `COPILOT_HOME`. `GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN` and `GH_HOST` are dropped too, unless `--keep-api-keys`. No GitHub token is ever set. A child whose `ATTICDB_HOME` is not under its run root is refused.
- A run touches only its direction's 2 harnesses. A Claude and Copilot run never sets up or disconnects Codex, and needs no Codex login.
- Before and after a batch, the real AtticDB home, the top level of `~/.claude*`, `~/.codex` and `~/.copilot`, and VS Code's user `mcp.json` are compared by stat only. A change prints a warning.

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

### Copilot CLI

Checked on Copilot CLI 1.0.105.

- `COPILOT_HOME` moves all of Copilot's config and state: `config.json`, `logs/`, `session-state/`, `session-store.db`. The login is not in it. It is in the OS keychain, so an empty `COPILOT_HOME` is logged in. Log in once with `copilot login`.
- The guard refuses a run `COPILOT_HOME` that is, or lies in, `~/.copilot`, through any symlink, as for `~/.claude*` and `~/.codex`. It checks before the dir is made and again after.
- A session is `copilot -p <prompt> --output-format json --allow-all-tools --no-auto-update --model <id>`. stdout is JSONL. `parse.parse_copilot` reads it.
- **The hook needs no `COPILOT_ALLOW_ALL`.** `atticdb setup --yes --harness copilot` writes `mcp-config.json`, `hooks/atticdb.json` and `instructions/atticdb.instructions.md` in `COPILOT_HOME`. In a real `-p` run with `COPILOT_ALLOW_ALL` unset, the user-level sessionStart hook ran: `session-state/<id>/events.jsonl` had `hook.start` and `hook.end` with `"success": true` and the AtticDB block, and the model quoted a fact from that block without a tool call. So the eval doesn't set it. Its value `true` would also trust the repo and load the repo's own hooks. All inherited `COPILOT_*` vars are dropped.
- The hook command is the one `atticdb connect copilot` writes: `'<atticdb>' hook session-start`, with no `--harness`. It prints `{"additionalContext": ...}`.
- Copilot's JSON stream has no hook events. So after each Copilot session in the atticdb and atticdb-only arms, the run reads the `hook.end` events in `COPILOT_HOME/session-state/` and adds a note to the reasons, such as `copilot reader: sessionStart hook ran in 1 of 1 sessions, block has [LEDG-7731,412]`.
- `atticdb connect copilot` drops an atticdb entry it finds in VS Code's user `mcp.json`, which it finds through `HOME` (`~/Library/Application Support/Code/User/mcp.json` on macOS) or `XDG_CONFIG_HOME` elsewhere. So every copilot `atticdb setup` and `atticdb disconnect` step runs with `HOME=<harness-root>/home` and `XDG_CONFIG_HOME=<harness-root>/home/.config`, a new empty dir per run. It only ever sees a per-run `mcp.json`. 2 backstops stay: in the atticdb and atticdb-only arms, a run that sets up Copilot errors at `setup` if the real file mentions atticdb, and the batch compares the real file by stat.
- **Copilot agent sessions keep the real `HOME`.** The plan was a per-run `HOME` for them too, so that personal skills in `~/.agents/skills` or `~/.copilot/skills` can't load. But Copilot's keychain login doesn't work with another `HOME`. With `HOME=<empty dir>` and `XDG_CONFIG_HOME` under it, `copilot -p` exits 1 with "Error: No authentication information found", with or without `GH_CONFIG_DIR` pointing at the real gh config. With the real `HOME` and gh's config hidden, it logs in, so the login is Copilot's own keychain item, which it reaches only through the real `HOME`. That leaves a possible confound between arms: personal skills. Checked in a discovery session on 1.0.105 with the real `HOME` (this machine has 103 dirs in `~/.agents/skills`): the system message's `<available_skills>` held only the 2 builtin skills, `customize-cloud-agent` and `github-pr-media`. `copilot skill list` shows the same 2. So no personal skill loaded. Every run still reads the skills each Copilot session was offered from `COPILOT_HOME/session-state/*/events.jsonl`, and any non-builtin one adds a note such as `copilot reader: 1 non-builtin skills offered (<name>)`. A run with that note is confounded.
- Copilot loads AGENTS.md and CLAUDE.md from the repo, as `copilot instruction list` shows, so the files arm works unchanged.
- **Native memory.** Copilot's memory is off in prompt mode unless `--enable-memory`, and `copilot memories` imports to a service, not a local store. So capture scans no Copilot memory dir. `session-store.db` holds the writer's session history. It's not memory a reader loads, so it's deleted with the rest of the writer's dir before the reader, like the other writer transcripts.
- Each session's `premiumRequests` goes in the reasons, such as `writer 1 premium_requests=1`.
- `--check` only runs `copilot --version` from a run's own `COPILOT_HOME`. It does not check the Copilot login: Copilot CLI has no login status command. A logged-out keychain shows as an errored writer or reader, with "No authentication information found" in the reasons.

### Clean start

Each run starts clean, and checks that it did:

1. Both of the run's harnesses are disconnected from AtticDB. Outside the atticdb and atticdb-only arms, a failed disconnect errors the run at `setup`.
2. No config the run will use may mention atticdb: `settings.json` and `.claude.json` in the Claude dir, `config.toml`, `hooks.json` and `AGENTS.md` in the Codex dir, `mcp-config.json`, `settings.json`, `config.json`, `hooks/atticdb.json` and `instructions/atticdb.instructions.md` in the Copilot dir. Codex's `[projects."<path>"]` trust headers are skipped. A hit errors the run at `setup`.
3. The store is searched for each evidence token. The repo and the run's 3 config dirs are scanned too, except `auth.json`. A hit errors the run at `start`.

No agent runs in a run that errored there.

After each run: `atticdb disconnect` for both of the run's harnesses, delete `<harness-root>`, stop the AtticDB server the run started, delete the root.

## Arms

| Arm | What the writer and reader get |
|---|---|
| native | The repo files only. |
| files | AGENTS.md and CLAUDE.md in the fixture say: "Keep HANDOFF.md at the repo root. After each task, record decisions, their reasons, rejected approaches and constraints. Read it before you start." Nothing is pre-filled. The writer has to keep the file. |
| atticdb | `atticdb setup --yes --harness <h>` for both of the run's harnesses before the writer. Both are disconnected after the reader. |
| oracle | No writer session. The reader prompt starts with "Notes from the last session: " and the case's gold note. A case whose writer changes code has an `oracle_state` in cases.py. Its files go over the fixture and are committed as "writer work" before the reader, so the repo matches the note. Only H1 and H5 have one: `pack_header` packs with `struct.pack`. |
| atticdb-only | The atticdb arm with each harness's own memory switched off. It separates AtticDB's effect from the harness's memory. Writer and reader get the switch. If the writer still writes native memory, the run adds "native memory written with memory off" to its reasons. Runs only when named in `--arms`. |
| blank | No writer session and no notes. The reader gets the plain reader prompt. It is a control: its pass rate is the chance floor, and it flags leaks. `leaked` is 1 when an evidence token is in the reader's reply or tool outputs. Other arms leave `leaked` empty. Runs only when named in `--arms`. |

In the files arm, AGENTS.md and CLAUDE.md are committed with the fixture. So they are not part of any diff.

Each writer prompt is its own headless session. H3 has 2. Writers run in harness A's eval config dir, the reader in harness B's.
The commands come from `setup_eval.agents.agent_argv`, with the model flag added:

- `claude`: `--model <id>`, last.
- `copilot`: `--model <id>`, last.
- `codex exec`: `-m <id>`, just before the prompt. The prompt stays the last argument.

Outside the atticdb and atticdb-only arms, Codex runs without the `-c mcp_servers.atticdb.tools.fact_set.approval_mode=...` override.
With no atticdb server configured, that override alone makes a partial server table.
Codex 0.147 then refuses to load its config: "invalid transport in `mcp_servers.atticdb`".

The atticdb-only arm switches off each harness's own memory, in every writer and reader session.
The switches were checked on 2026-11-05:

- Claude Code: the env var `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`.
- Codex 0.147: `--disable memories`, just before `-m <id>`.
- Copilot: nothing. Its memory is off in prompt mode unless `--enable-memory`. No arm passes that flag.

## Stages

Each stage is 1, 0, or empty. Empty means not applicable, or not measured because the run errored first.

Every match goes through `EVIDENCE_PATTERNS` in run.py, 1 pattern per token. Capture, delivery and the clean start all use it:

| Token | Counts as |
|---|---|
| `LEDG-7731`, `INC-2209`, `STOR-5150`, `bramble-ci`, `FIN-88` | the whole token, any case, with no letter or digit next to it |
| `412` | `412 bytes` or `412-byte`. A bare 412, such as a file size of 4120, doesn't count |
| `3.9` | `Python 3.9` as words, or 3.9 within 40 chars of `bramble-ci`. `Python 3.9.6`, `python3.9` and a path part such as `/python 3.9/` or `3.9/site-packages` don't count |
| `plain files` | `plain file`, `plain-file` or `plain files` |

**captured**, after the writer sessions. True when any of the case's evidence tokens is found in 1 of 3 places:

1. The run's AtticDB store: `atticdb search --query <token>`, matched on each hit's content and key.
2. Any file in the repo working tree, tracked or not. It skips `.git`, `.venv`, `venv`, `__pycache__`, `.pytest_cache`, `node_modules` and any dir holding `pyvenv.cfg`.
3. The writer harness's native memory for this repo, in the run's own config dir. Only files changed during the run count:
   - Claude: `<CLAUDE_CONFIG_DIR>/projects/<slug>/memory/`. The slug is the repo's real path with every character that is not a letter or digit turned into `-`. That is how Claude names it. `/private/var/x_y/evalrepo` gives `-private-var-x-y-evalrepo`.
   - Codex: `<CODEX_HOME>/memories/`.
   - Copilot: none. See "Copilot CLI".

`captured_where` lists each place with the tokens found there, such as `atticdb:fact repo/evalrepo/x[LEDG-7731]; repo:HANDOFF.md[LEDG-7731,412]`.
A store fact is named by its key, and a store turn by its lsn, such as `atticdb:turn lsn 7`. Each saved note is 1 turn.
The oracle has no capture.

**captured_all**, "captured with reason": every evidence token of the case sits in 1 of those places. For H1 that is `LEDG-7731` and `412` in 1 note, fact or file, so the decision was saved with its reason. Tokens split over 2 notes don't count. H2 and H6 have 1 token, so there it equals captured.

**delivered**, before the reader's final reply. True when a token was in its context:

- `hook`: the block that the reader's hook command prints just before the reader starts: `atticdb hook session-start --harness <reader>`, or for Copilot `atticdb hook session-start`. atticdb and atticdb-only arms only.
- `transcript`: a tool result or file read in the reader's raw stream, before its final assistant message. Claude: `tool_result` content, and a Read's `tool_use_result.file.content`. Write and Edit results are left out, as they echo what the reader wrote. Codex: command output and MCP results. The commands the agent typed don't count. Copilot: the `result` of each `tool.execution_complete`, except for `edit`, `create`, `write`, `str_replace_editor` and `apply_patch`. The streamed `tool.shell_output` and partial results repeat it, so they're not read. The reader's own messages, the prompt and tool arguments never count.
- `prompt`: the oracle's reader prompt.

H5 has no delivery stage. Its evidence is H1's tokens, which are unrelated history there.

**passed**: `grade.grade` on the reader's final reply, `git diff refs/eval/base`, and the repo it leaves.
`check_timed_out` is 1 when a code check timed out. grade.py has no flag of its own yet, so its reasons are read for "timed out". A fail with it also gets "check timed out" in the reasons.

**errored**: a session that timed out, did not start, exited non-zero, reported an error, or gave no final reply.
The reason names errors from the JSON stream first, such as Codex's `turn.failed`, Claude's failed `result`, or Copilot's `session.error` or non-zero `exitCode`. Then the last stderr line. A bad Copilot `--model` prints only to stderr and exits 1.
Stderr often holds only a banner, such as "Reading additional input from stdin...".
`error_stage` says where: `fixture`, `setup`, `start`, `writer`, `capture`, `hook`, `reader`, `grade`, `budget` or `teardown`.
An errored run is kept in the CSV. It's never a pass or a fail, and the batch goes on.

## Cost

A run is its writer sessions plus 1 reader. The oracle and blank arms have no writer.
The pilot matrix is 6 cases, 4 arms, 2 directions and 1 rep: 48 runs and 90 agent sessions. 42 are writers and 48 are readers.
That holds for Claude and Codex, and for Claude and Copilot. Each Copilot session costs premium requests. A checked session on `gpt-5.6-terra` made 3 model calls and cost 1. Other models have other multipliers.
Each session has a 300 s timeout by default (`--agent-timeout`).

`--max-sessions` is required for a real run. It counts writer and reader sessions both.
A batch over it is refused before anything starts.
While a batch runs, it stops before a run that would pass it. A session past it never starts.

## Run

Log in once, as in setup_eval:

```sh
CLAUDE_CONFIG_DIR=~/.cache/atticdb-setup-eval/claude claude auth login
CODEX_HOME=~/.cache/atticdb-setup-eval/codex codex login
copilot login   # only for a direction with copilot; the login goes in the OS keychain
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

Claude and Copilot:

```sh
python3 -m research.handoff_eval --cases H1,H2,H3,H4,H5,H6 --arms native,files,atticdb,oracle \
    --directions claude-code:copilot,copilot:claude-code --reps 1 --claude-model <id> --copilot-model <id> \
    --max-sessions 90 --atticdb target/release/atticdb --out research/handoff_eval/results/<date>-pilot.csv
```

`--check` is setup_eval's check. It runs `claude auth status` and `codex login status` on the eval dirs, and prints only whether each is logged in.
Then it runs both again from a run's own config dirs. For Copilot it only runs `copilot --version` from a run's own `COPILOT_HOME`, and does not check the login.
`--dry-run` prints every planned session, with its command, cwd, timeout and env names, then the total. It starts nothing and writes nothing.

| Flag | Default | Meaning |
|---|---|---|
| `--cases` | all 6 | a comma list of H1 to H6 |
| `--arms` | native, files, atticdb, oracle | a comma list of native, files, atticdb, oracle, atticdb-only, blank |
| `--directions` | `claude-code:codex,codex:claude-code` | a comma list of `writer:reader` with claude-code, codex or copilot. Same-harness directions are refused, as the pilot is cross-harness only |
| `--reps` | 1 | runs per case, arm and direction |
| `--claude-model`, `--codex-model`, `--copilot-model` | none | each is required only when the directions use its harness |
| `--max-sessions` | none | required for a real run |
| `--atticdb` | `target/debug/atticdb` | the AtticDB binary |
| `--out` | `research/handoff_eval/results/<date>.csv` | rows are appended. The summary goes to the same path with `.md`, for this batch only |
| `--agent-timeout` | 300 | seconds per agent session |
| `--keep-api-keys` | off | pass the credential and provider vars on |

Exit codes: 0 when every run finished, 1 when a run errored or the budget stopped the batch, 2 for a refusal or a bad flag.

## Results

The CSV has 1 row per run: case, arm, writer, reader, rep, the 3 stages and where each was found, captured_all, leaked, errored and its stage, sessions, tokens in and out, wall time, both model ids, the reasons and the reply.
Reasons hold the grader's reasons and a redacted excerpt of the reply, cut at 200 chars. `reply` holds the reader's whole final reply, redacted the same way: token shapes and the run's paths are replaced. No column holds an env value.
A batch refuses an `--out` CSV whose header differs from this version's, such as one from before `captured_all` and `reply`.

The summary's first line is "Integration pilot. Not a statistical result."
Then pass rates by arm and case, a stage funnel per arm with the oracle ceiling, captured with reason per arm, and pass rates by direction.
With blank rows, a "Control (blank arm)" section gives the blank pass rate and how many runs leaked.
Rates count runs that did not error. Errored runs are counted apart.

### Re-grade

```sh
python3 -m research.handoff_eval --regrade research/handoff_eval/results/2026-11-04-pilot.csv
```

It grades the stored answer replies (H1, H3, H4, H5) again with the current graders, prints each verdict that changed, then the summary. It runs no agent and writes nothing.
H4 passes only on a reply that starts with no and cites `bramble-ci` (whole token, any case), with no hedged version in the clause that names it and nothing that permits match. 3.9 tied to CI, the repo or HANDOFF.md without `bramble-ci` fails, so a re-grade prints "H4 passes only when the reply cites bramble-ci. Hand-audit H4 fails."
Code cases (H2, H6) keep their stored verdict, as the repo they were graded on is gone. Errored rows stay errored. captured_all is worked out again from `captured_where`.
A CSV without the `reply` column, such as the 2026-11-04 pilot, is graded on the `reply=` excerpt in the reasons. That excerpt is flattened and cut at 200 chars, so the output says those replies may be cut off, and marks each changed verdict that was graded on a cut reply.
In such a CSV every store turn is the 1 place `atticdb:turn`, so 2 notes there count as 1 place. The output counts the rows this affects.
