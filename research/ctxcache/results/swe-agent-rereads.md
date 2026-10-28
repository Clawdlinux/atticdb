# SWE-agent trajectories (nebius), re-reads only

Data: [nebius/SWE-agent-trajectories](https://huggingface.co/datasets/nebius/SWE-agent-trajectories). Nebius, SWE-agent-trajectories, CC-BY-4.0. Only aggregate counts appear here.
Sample: 1000 rows. The 80,036 rows are cut into 1000 equal strata, and each gives 1 random row. Seed 87.
Run: `python3 -m research.ctxcache --dataset swe-agent --drop-refs --sample 1000 --seed 87`

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 1,000 (skipped: 0 truncated) |
| Accesses | 23,852: 17,803 first, 6,049 re-reads, 0 references |
| Compulsory share (first accesses) | 74.6% of accesses, 76.2% of access tokens |
| Object tokens by kind | file 74.4%, output 25.5%, url 0.1% |
| Dead tokens (objects never accessed again) | 91.5% of object tokens. Files 92.7%, outputs 88.1% |
| Reuse distance p50 / p90 / p99 | 98 / 1,969 / 6,983 tokens |
| Peak live tokens per trajectory, p50 / p90 | 0 / 1,461. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90 | 6,589 / 21,315. The context with no eviction |
| Message tokens, all trajectories | 3,940,279, outside the model (tool output: 9,730,724) |
| Uses of a file with no view of its version | 9, not accesses |

## Missed tokens, % of re-accessed tokens

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 50.8 | 51.0 | 46.8 | 51.5 | 28.5 | 36.8 |
| 2K | 22.4 | 22.6 | 16.6 | 26.6 | 2.6 | 8.4 |
| 4K | 9.5 | 10.1 | 5.3 | 10.1 | 0.0 | 1.4 |
| 8K | 1.4 | 1.7 | 0.0 | 1.8 | 0.0 | 0.1 |
| 16K | 0.0 | 0.0 | 0.0 | 0.1 | 0.0 | 0.0 |
| 32K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Misses, % of re-accesses

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 28.6 | 29.3 | 25.4 | 27.1 | 10.0 | 15.2 |
| 2K | 13.2 | 13.7 | 9.9 | 12.0 | 0.8 | 3.4 |
| 4K | 6.0 | 6.5 | 3.8 | 4.2 | 0.0 | 0.6 |
| 8K | 0.9 | 1.2 | 0.0 | 0.6 | 0.0 | 0.0 |
| 16K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 32K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing | GDSF |
|---|---|---|---|---|
| 1K | 1.38x to 1.78x | 1.39x to 1.79x | 1.27x to 1.64x | 1.40x to 1.80x |
| 2K | 2.65x to 8.66x | 2.68x to 8.74x | 1.97x to 6.44x | 3.16x to 10.32x |
| 4K | 6.87x to 621.26x | 7.24x to 654.75x | 3.81x to 344.04x | 7.29x to 658.63x |
| 8K | at least 27.67x | at least 34.63x | at least 0.81x | at least 35.78x |
| 16K | OPT misses 0 | OPT misses 0 | none missed | OPT misses 0 |
| 32K | none missed | none missed | none missed | none missed |
| 64K | none missed | none missed | none missed | none missed |
| 128K | none missed | none missed | none missed | none missed |

Clearing keeps the newest N tool outputs. Median N per budget: 1K: 5, 2K: 6, 4K: 11, 8K: 15, 16K: 15, 32K: 15, 64K: 15, 128K: 15.
N stops at the trajectory's count of tool outputs once everything fits.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 15.8% of re-accessed tokens missed (21.2% of re-accesses), 1,539 tokens held on average.
- Keep everything: no misses, 4,874 tokens held on average.
