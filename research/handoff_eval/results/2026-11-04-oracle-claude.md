Integration pilot. Not a statistical result.

Runs: 6. Errored: 0. Agent sessions: 6.
Models: claude-code=claude-opus-5-5, codex=gpt-5.6-terra.

## Pass rate by arm and case

| Arm | H1 | H2 | H3 | H4 | H5 | H6 | Errored |
|---|---|---|---|---|---|---|---|
| oracle | 0/1 | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 | 0 |

## Stage funnel per arm

Each stage counts runs that did not error and where the stage applies. Oracle has no capture. H5 has no delivery. Oracle ceiling is the oracle arm's pass rate on the same cases.

| Arm | Captured | Delivered | Passed | Oracle ceiling | Errored |
|---|---|---|---|---|---|
| oracle | n/a | 5/5 | 5/6 | 5/6 | 0 |
