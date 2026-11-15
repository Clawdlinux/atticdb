# stale_rule

A rule says "do X". The code does Y. Is the rule stale, or is it a migration the code hasn't caught up with?
The working tree can't tell you. Only history can.

This package measures what coding agents do in that spot, and what fixes it.
Design: [docs/plans/2026-11-15-stale-or-migration.md](../../docs/plans/2026-11-15-stale-or-migration.md).

Python 3, standard library only. Tests use pytest and never start an agent.

## What a run is

- 4 tiny Python projects, 1 per convention: errors, timestamps, JSON keys, ids.
- Each project has 2 git histories with byte-identical final trees:
  - **stale**: the rule came first, then the code moved away from it
  - **migration**: the code came first, then the rule was added
- 2 channels for the rule:
  - **file**: a line in the repo's `CLAUDE.md`, tracked in git
  - **memory**: a SessionStart hook block, as AtticDB prints it. The repo never holds the rule
- Arms are text in the user-level `CLAUDE.md`, or how the memory line is shown. See `fixtures.py`.
- `--neutral` makes the stale refactor commit's message name only its files. Then dates are the only clue.
- The agent is `claude -p --model sonnet`, edits accepted, Bash for `git`, `python3` and `pytest` only. No MCP. Auto memory off. 600 s cap.
- Grading is mechanical, on the lines added under `app/`: `rule`, `code`, `mixed` or `none`. Correct is `code` for stale and `rule` for migration.

## Isolation

- Each run gets a new temp project and a new `CLAUDE_CONFIG_DIR`. Both are deleted after.
- Login comes from the eval dir through `CLAUDE_SECURESTORAGE_CONFIG_DIR`, as in `handoff_eval`.
- `ANTHROPIC_*`, `CLAUDE_*`, `GIT_*`, `ATTICDB_*`, `OPENAI_*` and `AWS_*` are dropped. Git runs with no user config.
- The init event shows no MCP servers and only built-in skills.
- Raw transcripts go to `~/.cache/atticdb-stale-rule/<batch>/`, never into the repo.

## Run

```sh
PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest -q research/stale_rule/tests
python3 -m research.stale_rule run --batch my-batch --channel memory --reps 2 --jobs 4
python3 -m research.stale_rule summary research/stale_rule/results/my-batch.csv
```

A batch that hits the login's usage limit records errored rows. Rerun only those with `--resume <csv> --out <new csv>`.
