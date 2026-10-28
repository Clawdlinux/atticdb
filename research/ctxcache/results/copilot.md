# Our Copilot CLI sessions

Data: our own Copilot CLI session logs, main agent only. Aggregates only.
Run: `python3 -m research.ctxcache --dataset copilot`

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 21 (skipped: 2 fork) |
| Accesses | 6,400: 5,964 first, 27 re-reads, 409 references |
| Compulsory share (first accesses) | 93.2% of accesses, 86.1% of access tokens |
| Object tokens by kind | file 27.4%, output 67.3%, url 5.2% |
| Dead tokens (objects never accessed again) | 90.5% of object tokens. Files 67.0%, outputs 100.0% |
| Reuse distance p50 / p90 / p99 | 3,499 / 107,083 / 267,568 tokens |
| Peak live tokens per trajectory, p50 / p90 | 0 / 8,002. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90 | 9,278 / 427,863. The context with no eviction |
| Message tokens, all trajectories | 495,809, outside the model (tool output: 2,115,144) |
| Uses of a file with no view of its version | 778, not accesses |
| Compactions | 37 |

## Missed tokens, % of re-accessed tokens

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 90.0 | 90.3 | 90.2 | 91.7 | 70.2 | 79.0 |
| 2K | 72.5 | 74.6 | 74.0 | 78.2 | 27.5 | 49.8 |
| 4K | 52.8 | 54.8 | 63.3 | 68.6 | 5.5 | 27.7 |
| 8K | 44.6 | 47.6 | 54.4 | 54.0 | 0.3 | 9.5 |
| 16K | 35.6 | 36.8 | 45.1 | 45.6 | 0.0 | 2.6 |
| 32K | 27.2 | 27.6 | 36.9 | 35.9 | 0.0 | 0.0 |
| 64K | 16.8 | 18.4 | 26.1 | 25.1 | 0.0 | 0.0 |
| 128K | 7.5 | 7.9 | 14.7 | 13.1 | 0.0 | 0.0 |

## Misses, % of re-accesses

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 79.4 | 79.8 | 82.3 | 80.0 | 38.3 | 53.2 |
| 2K | 64.9 | 66.7 | 72.2 | 64.2 | 13.3 | 33.3 |
| 4K | 50.7 | 53.0 | 64.2 | 53.9 | 3.7 | 20.4 |
| 8K | 42.0 | 44.7 | 56.0 | 41.5 | 0.2 | 7.6 |
| 16K | 33.9 | 34.9 | 46.1 | 32.3 | 0.0 | 1.6 |
| 32K | 23.9 | 24.8 | 38.3 | 24.5 | 0.0 | 0.0 |
| 64K | 13.8 | 16.7 | 28.0 | 16.3 | 0.0 | 0.0 |
| 128K | 7.6 | 8.3 | 13.1 | 7.8 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing | GDSF |
|---|---|---|---|---|
| 1K | 1.14x to 1.28x | 1.14x to 1.29x | 1.14x to 1.29x | 1.16x to 1.31x |
| 2K | 1.45x to 2.64x | 1.50x to 2.72x | 1.48x to 2.69x | 1.57x to 2.85x |
| 4K | 1.90x to 9.65x | 1.98x to 10.01x | 2.28x to 11.57x | 2.47x to 12.52x |
| 8K | 4.68x to 172.84x | 5.00x to 184.51x | 5.71x to 210.70x | 5.67x to 209.26x |
| 16K | at least 13.43x | at least 13.89x | at least 17.02x | at least 17.22x |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 64K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 128K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |

Clearing keeps the newest N tool outputs. Median N per budget: 1K: 4, 2K: 4, 4K: 8, 8K: 15, 16K: 16, 32K: 16, 64K: 16, 128K: 16.
N stops at the trajectory's count of tool outputs once everything fits.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 72.1% of re-accessed tokens missed (73.4% of re-accesses), 1,774 tokens held on average.
- Keep everything: no misses, 332,771 tokens held on average.
- Observed: 12 re-reads in the same compaction window (6,695 tokens), 15 after a compaction (9,645 tokens).
