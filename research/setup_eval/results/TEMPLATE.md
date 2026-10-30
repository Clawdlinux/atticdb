# AtticDB setup eval, <date>

Fill this in after a real batch. The method is in [../README.md](../README.md).

## What was run

- AtticDB: commit `<sha>`, binary `<path>`.
- Claude Code `<claude --version>`, model `<model>`.
- Codex `<codex --version>`, model `<model>`.
- Command: `python3 -m research.setup_eval run <flags>`.
- Runs: `<n>` of the 36-run budget. CSV: [`<date>.csv`](<date>.csv).
- Each run: a new `ATTICDB_HOME` and fixture repo, `atticdb setup --yes --harness <h>` against the eval config dirs, then `atticdb disconnect <h>`.
- The real AtticDB home's data dir: `<unchanged, or the warning>`.

## Eval-only overrides

- Claude Code: `--allowedTools=mcp__atticdb__fact_set`, as nobody can answer a prompt.
- Claude Code: `--permission-mode acceptEdits`, so S4 tests memory, not the permission prompt for the edit.
- Codex: `--dangerously-bypass-hook-trust`, and `-c 'mcp_servers.atticdb.tools.fact_set.approval_mode="approve"'` to approve `fact_set`.
- Codex: `--sandbox workspace-write`, named explicitly. A fresh config has no trust entry for the fixture, so Codex 0.147 would pick read-only. It is the plan's default sandbox, which still tests that the MCP server reaches the home socket.
- Credential and provider vars stripped from the env: `<yes, or --keep-api-keys>`.
- `<any other change from the plan>`

## Claude Code

| Scenario | Pass | AtticDB calls, median | Input tokens, median | Output tokens, median | Turns, median | Wall s, median |
|---|---|---|---|---|---|---|
| S1 recall | | | | | | |
| S2 search | | | | | | |
| S3 save | | | | | | |
| S4 summary | | | | | | |
| B1 baseline for S1 | | | | | | |
| B2 baseline for S2 | | | | | | |

## Codex

| Scenario | Pass | AtticDB calls, median | Input tokens, median | Output tokens, median | Turns, median | Wall s, median |
|---|---|---|---|---|---|---|
| S1 recall | | | | | | |
| S2 search | | | | | | |
| S3 save | | | | | | |
| S4 summary | | | | | | |
| B1 baseline for S1 | | | | | | |
| B2 baseline for S2 | | | | | | |

## Notes

- `<runs with parse_ok 0, setup_exit other than 0, or timeouts, and why>`
- `<did the instructions change after the first pass?>`
