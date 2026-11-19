# LoCoMo retrieval at equal text budgets

Top-k scores give both retrievers the same number of turns, but those turns can differ in length.
Here both get the same byte ceiling. We count recall and how often the full labelled evidence fits.

| Slice | Budget | AtticDB recall | SQLite recall | AtticDB complete | SQLite complete |
|---|---:|---:|---:|---:|---:|
| All (1,536) | 1,024 B | 54.1% | 51.9% | 49.1% | 47.6% |
| All (1,536) | 2,048 B | 60.6% | 58.7% | 55.1% | 53.7% |
| Temporal (321) | 1,024 B | 63.2% | 62.0% | 60.1% | 58.6% |
| Temporal (321) | 2,048 B | 70.7% | 69.2% | 67.6% | 66.0% |

Reported quality scores matched to 1 decimal place across 3 fresh-store runs of the same questions.
These are repeat runs, not 3 independent datasets.
At 1,024 B, mean bytes used over all questions were 920 for AtticDB and 938 for SQLite.
At 2,048 B, means were 1,943 and 1,794.

Search p95 ranged from 0.62 to 0.69 ms for AtticDB and 0.24 to 0.26 ms for SQLite.
AtticDB went through a Unix socket. SQLite ran in process. Timing excludes ingest and startup.

## Reproduce

- 2026-11-19 IST. Apple M4, arm64, macOS 27.2.1, 10 CPUs.
- AtticDB release-profile build at `7dfa70caee87a13649bce758ad51339383c770f7`; SQLite 3.54.1 with FTS5.
- Seed 0, 10 conversations, dataset SHA-256 `79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4`.
- Run the [harness](../README.md#how-to-run-it) from the checkout containing this report.
- `results.csv` includes the input questions and stays out of Git.

## Not measured

These rows do not rank Mem0 or Walrus Memory. They do not measure QA, semantic recall, or code correctness.
The next comparison should ask for both current and historical decisions after a correction.
Use pinned versions, the same history and text budget, and report when old evidence is mistaken for current evidence.
