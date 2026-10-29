# setup_eval

Does `zerodb setup` make agents use memory, with the user-level files exactly as shipped?
This harness runs Claude Code and Codex headless on a small fixture repo and checks.
It is the headless eval of Task 7 in the setup plan.

The package is `research/setup_eval`, with an underscore so it imports.
The plan and the brief call it `research/setup-eval`; that path does not exist.

Python 3, standard library only. Tests use pytest and stub agents.

## Isolation

The eval uses 2 config dirs of its own:

- `~/.cache/zerodb-setup-eval/claude`, as `CLAUDE_CONFIG_DIR`.
- `~/.cache/zerodb-setup-eval/codex`, as `CODEX_HOME`.

They must hold no other hooks, plugins, MCP servers or instructions.
The harness refuses to run if either one is, or lies in, `~/.claude*`, `~/.codex` or `~/.copilot`, through any symlink.

Each run gets a new temp root with:

- its own zerodb home, `<root>/zh`, as `ZERODB_HOME`;
- a new fixture repo, `<root>/evalrepo`: a `calc.py` with 1 function, a README with no hint of the answers, and 1 commit.

Then it seeds the store, and runs `zerodb disconnect <h>` and `zerodb setup --yes --harness <h>`.
`HOME` stays the real one, so setup edits only the eval config dirs.
It names 1 harness at a time, so `connect vscode` and `connect copilot` never run.
After the agent, it runs `zerodb disconnect <h>`, stops the zerodb server the run started, and deletes the root.

Every child gets the inherited env without `ZERODB_*`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME`, then the run's own values.
It also drops `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY` and `CODEX_API_KEY`, so the agents bill the logins in the eval dirs.
`--keep-api-keys` passes them on.
The harness refuses to start any child whose `ZERODB_HOME` is not under its run root.
The harness's own git calls skip the user's git config.

The real zerodb home is never used.
Before and after a batch, the harness records the size and mtime of each file in its `data/` dir.
On macOS that is `~/Library/Application Support/zerodb/data`. On Linux it is `$XDG_DATA_HOME/zerodb/data`, else `~/.local/share/zerodb/data`.
It only stats the files and never opens one.
If any changed, it prints a warning. That is not a failure, as another session may write there.

## Log in once

```sh
mkdir -p ~/.cache/zerodb-setup-eval/claude ~/.cache/zerodb-setup-eval/codex
CLAUDE_CONFIG_DIR=~/.cache/zerodb-setup-eval/claude claude auth login
CODEX_HOME=~/.cache/zerodb-setup-eval/codex codex login
```

## Run

```sh
cargo build -p zerodb
python3 -m research.setup_eval check
python3 -m research.setup_eval run --zerodb target/debug/zerodb --dry-run
python3 -m research.setup_eval run --harness both --scenarios S1,S2,S3,S4,B1,B2 --runs 1 --zerodb target/debug/zerodb
python3 -m pytest research/setup_eval/tests
```

The end-to-end test runs the real zerodb with stub agents. It uses `$ZERODB_BIN`, else `target/debug/zerodb`, and is skipped when neither exists.

`check` runs `claude auth status` and `codex login status` on the eval dirs, 20 s each.
It prints only whether each is logged in, never their output.
It is the only command that may start the real binaries without a run.

`--dry-run` prints every command of every run, with its cwd, timeout and env names.
It also names the API keys it strips. It starts nothing and writes nothing. Env values are not shown.

| Flag | Default | Meaning |
|---|---|---|
| `--harness` | `both` | `claude-code`, `codex` or `both` |
| `--scenarios` | `S1,S2,S3,S4,B1,B2` | a comma list |
| `--runs` | 1 | runs per harness and scenario |
| `--zerodb` | | the zerodb binary to set up and seed with |
| `--out` | `research/setup_eval/results/<date>.csv` | rows are appended |
| `--dry-run` | off | print the plan, run nothing |
| `--keep-api-keys` | off | pass the 4 API keys on, so the agents may bill them |
| `--max-runs` | 12 | refuse a larger batch. At most 36, the plan's whole budget |

Exit codes: 0 when the batch ran, 1 when a run hit a harness error, 2 for a refusal or a bad flag.

## Scenarios

| Id | Seed | Prompt | Pass |
|---|---|---|---|
| S1 | fact `repo/evalrepo/test-runner`: cargo nextest run | How do I run this repo's tests? 1 line. | the reply says `nextest` |
| S2 | 1 note on why Redis was dropped, then 10 filler notes | Why did we drop Redis here? 1 line. | a `memory_search` call, and the reply gives the reason |
| S3 | none | Remember for this repo: always pin Node 22. | a `fact_set` under `repo/evalrepo/`, and the next block shows it |
| S4 | none | Add `add(a, b)` to `calc.py`. | `calc.py` gained `def add(a, b)`, a `memory_append` without `session`, and the next block shows the note |
| B1 | none | the S1 prompt | the S1 rule |
| B2 | none | the S2 prompt | the S2 rule |

The S2 note is: "We dropped Redis because its 2 extra round trips per append broke the 1 ms p99 budget."
The hook shows only the 5 newest notes, so the 10 fillers push it out of the block.

Every text match ignores case and is a plain substring match:

- S1: `nextest` in the reply.
- S2: `round trip`, `round-trip`, `roundtrip` or `p99` in the reply.
- S3: a block line `- <key>: ...` holds `22`, for a key the agent set under `repo/evalrepo/`.
- S4: an added line of `git diff <first commit> -- calc.py` holds `def add(a, b)`. The `memory_append` has no `session`, or an empty or null one, as zerodb reads those as none. The first 60 characters of the note, with whitespace collapsed, appear in the block.

For S3 and S4, the next block is what `zerodb hook session-start --harness <h>` prints after the agent.
It gets `{"cwd": "<fixture>", "hook_event_name": "SessionStart"}` on stdin and the run's `ZERODB_HOME`.

A pass also needs the agent to finish cleanly: exit 0 within the timeout, a final reply, and no error it reported.
Unparsed lines that don't stop the final reply from being found count only toward `parse_ok`.
If setup exits other than 0, the agent does not run, and the run fails. The notes say why.

## Agent commands

Claude Code, from the fixture repo, with a 300 s timeout:

```sh
claude -p <prompt> --output-format stream-json --verbose --permission-mode acceptEdits --allowedTools=mcp__zerodb__fact_set
```

`--allowedTools` takes a list. So the prompt comes right after `-p`, and the flag uses the `=` form, last.
`--permission-mode acceptEdits` comes before it, so `acceptEdits` is never read as a tool name.

Codex:

```sh
codex exec --json --dangerously-bypass-hook-trust -c 'mcp_servers.zerodb.tools.fact_set.approval_mode="approve"' --skip-git-repo-check <prompt>
```

Codex keeps its default `workspace-write` sandbox.
Each agent runs in its own process group. On timeout the group is killed.

Eval-only overrides, which the results must list:

- Claude Code: `--allowedTools=mcp__zerodb__fact_set`, as nobody can answer a prompt.
- Claude Code: `--permission-mode acceptEdits`, so S4 tests memory, not the permission prompt for the edit.
- Codex: `--dangerously-bypass-hook-trust`, and `-c` to approve `fact_set`.
- Both: no API keys in the env, unless `--keep-api-keys`.

## Parsing

Both streams are JSONL, parsed line by line.
A line that is not a JSON object, an event whose `type` is not a string, or a known event with fields of the wrong shape, counts as unparsed.
An event of an unknown type counts as unknown. Neither stops the parse.
`parse_ok` holds when the final reply was found and no line was unparsed.

- Claude Code: `tool_use` blocks in `assistant` messages, and the final `result` event with `result`, `usage` and `num_turns`.
- Codex: `mcp_tool_call` items with `server` and `tool`, the last `agent_message` item, and `usage` from each `turn.completed`. Older `{"msg": ...}` events are read too.

Tool calls keep the bare zerodb tool name, such as `fact_set`.
`input_tokens` counts every prompt token, cache reads and writes included. `cache_read_tokens` is the part read from the cache.
Turns are Claude's `num_turns`, and Codex's count of `turn.completed`. They do not compare across harnesses.

## Results

Rows are appended to the CSV as each run ends, so a stopped batch keeps what finished.

Columns: date, harness, scenario, run, pass, zerodb_calls (JSON of name to count), input_tokens, output_tokens, cache_read_tokens, turns, wall_s, setup_exit, parse_ok, notes.

The notes hold the first 200 characters of the reply, never the whole reply.
Before that, anything shaped like a token is replaced by `[redacted]`, and paths from the env by labels such as `<root>`.
Exit codes: -1 is a timeout and 127 a missing program.

[results/README.md](results/README.md) is the write-up template.

## Cost

Each run is 1 headless agent session on the account in the eval dir.
The plan's budget is 36 runs at most:

- A first pass of 12: each of the 8 cells once, plus B1 and B2 on both harnesses.
- If the instructions change after it, all 12 again, twice: 24 runs.
- If not, each of the 8 cells once more: 8 runs.

`--max-runs` defaults to 12, so a larger batch must be asked for.
