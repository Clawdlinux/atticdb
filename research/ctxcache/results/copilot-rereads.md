# Our Copilot CLI sessions, re-reads only

Data: our own Copilot CLI session logs, main agent only. Aggregates only.
Run: `python3 -m research.ctxcache --dataset copilot --drop-refs`

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 21 (skipped: 2 fork) |
| Accesses | 5,991: 5,964 first, 27 re-reads, 0 references |
| Compulsory share (first accesses) | 99.5% of accesses, 99.2% of access tokens |
| Object tokens by kind | file 27.4%, output 67.3%, url 5.2% |
| Dead tokens (objects never accessed again) | 99.4% of object tokens. Files 97.9%, outputs 100.0% |
| Reuse distance p50 / p90 / p99 | 96,111 / 540,721 / 544,990 tokens |
| Peak live tokens per trajectory, p50 / p90 | 0 / 1,203. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90 | 9,278 / 427,863. The context with no eviction |
| Message tokens, all trajectories | 495,809, outside the model (tool output: 2,115,144) |
| Uses of a file with no view of its version | 778, not accesses |
| Compactions | 37 |

## Missed tokens, % of re-accessed tokens

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 96.6 | 96.6 | 96.0 | 96.4 | 50.7 | 73.3 |
| 2K | 83.6 | 83.6 | 76.5 | 89.0 | 0.0 | 28.6 |
| 4K | 82.7 | 82.7 | 76.3 | 82.6 | 0.0 | 5.0 |
| 8K | 76.1 | 82.6 | 76.1 | 82.6 | 0.0 | 0.0 |
| 16K | 76.1 | 76.1 | 76.1 | 76.1 | 0.0 | 0.0 |
| 32K | 76.1 | 76.1 | 76.1 | 76.1 | 0.0 | 0.0 |
| 64K | 54.2 | 54.2 | 38.6 | 51.0 | 0.0 | 0.0 |
| 128K | 38.6 | 38.6 | 26.2 | 27.7 | 0.0 | 0.0 |

## Misses, % of re-accesses

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 85.2 | 85.2 | 88.9 | 81.5 | 29.6 | 48.1 |
| 2K | 77.8 | 77.8 | 77.8 | 74.1 | 0.0 | 18.5 |
| 4K | 74.1 | 74.1 | 70.4 | 70.4 | 0.0 | 3.7 |
| 8K | 66.7 | 70.4 | 66.7 | 70.4 | 0.0 | 0.0 |
| 16K | 66.7 | 66.7 | 66.7 | 66.7 | 0.0 | 0.0 |
| 32K | 66.7 | 66.7 | 66.7 | 66.7 | 0.0 | 0.0 |
| 64K | 55.6 | 55.6 | 44.4 | 48.1 | 0.0 | 0.0 |
| 128K | 44.4 | 44.4 | 29.6 | 25.9 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing | GDSF |
|---|---|---|---|---|
| 1K | 1.32x to 1.91x | 1.32x to 1.91x | 1.31x to 1.89x | 1.31x to 1.90x |
| 2K | at least 2.93x | at least 2.93x | at least 2.68x | at least 3.12x |
| 4K | at least 16.44x | at least 16.44x | at least 15.16x | at least 16.41x |
| 8K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 16K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 64K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 128K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |

Clearing keeps the newest N tool outputs. Median N per budget: 1K: 4, 2K: 4, 4K: 8, 8K: 15, 16K: 16, 32K: 16, 64K: 16, 128K: 16.
N stops at the trajectory's count of tool outputs once everything fits.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 83.0% of re-accessed tokens missed (81.5% of re-accesses), 1,760 tokens held on average.
- Keep everything: no misses, 331,378 tokens held on average.
- Observed: 12 re-reads in the same compaction window (6,695 tokens), 15 after a compaction (9,645 tokens).
