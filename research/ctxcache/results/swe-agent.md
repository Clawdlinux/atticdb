# SWE-agent trajectories (nebius)

Data: [nebius/SWE-agent-trajectories](https://huggingface.co/datasets/nebius/SWE-agent-trajectories). Nebius, SWE-agent-trajectories, CC-BY-4.0. Only aggregate counts appear here.
Sample: 1000 rows. The 80,036 rows are cut into 1000 equal strata, and each gives 1 random row. Seed 87.
Run: `python3 -m research.ctxcache --dataset swe-agent --sample 1000 --seed 87`

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 1,000 (skipped: 0 truncated) |
| Accesses | 35,269: 17,803 first, 6,049 re-reads, 11,417 references |
| Compulsory share (first accesses) | 50.5% of accesses, 46.7% of access tokens |
| Object tokens by kind | file 74.4%, output 25.5%, url 0.1% |
| Dead tokens (objects never accessed again) | 56.9% of object tokens. Files 46.2%, outputs 88.1% |
| Reuse distance p50 / p90 / p99 | 285 / 1,723 / 5,586 tokens |
| Peak live tokens per trajectory, p50 / p90 | 1,129 / 2,140. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90 | 6,589 / 21,315. The context with no eviction |
| Message tokens, all trajectories | 3,940,279, outside the model (tool output: 9,730,724) |
| Uses of a file with no view of its version | 9, not accesses |

## Missed tokens, % of re-accessed tokens

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 66.6 | 67.1 | 59.5 | 67.5 | 41.2 | 48.1 |
| 2K | 16.1 | 19.5 | 25.1 | 24.8 | 1.7 | 6.1 |
| 4K | 4.4 | 5.9 | 9.0 | 5.8 | 0.8 | 1.4 |
| 8K | 0.5 | 1.2 | 1.0 | 1.2 | 0.0 | 0.0 |
| 16K | 0.0 | 0.1 | 0.0 | 0.2 | 0.0 | 0.0 |
| 32K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Misses, % of re-accesses

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 39.2 | 40.2 | 36.8 | 39.2 | 17.9 | 24.7 |
| 2K | 12.8 | 13.9 | 18.3 | 12.8 | 0.5 | 2.8 |
| 4K | 3.2 | 4.9 | 7.4 | 2.5 | 0.1 | 0.3 |
| 8K | 0.5 | 1.2 | 1.5 | 0.4 | 0.0 | 0.0 |
| 16K | 0.0 | 0.2 | 0.0 | 0.1 | 0.0 | 0.0 |
| 32K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing | GDSF |
|---|---|---|---|---|
| 1K | 1.38x to 1.62x | 1.39x to 1.63x | 1.24x to 1.44x | 1.40x to 1.64x |
| 2K | 2.64x to 9.37x | 3.19x to 11.33x | 4.11x to 14.58x | 4.07x to 14.45x |
| 4K | 3.15x to 5.55x | 4.28x to 7.53x | 6.50x to 11.44x | 4.15x to 7.31x |
| 8K | at least 34.71x | at least 84.18x | at least 73.32x | at least 88.66x |
| 16K | OPT misses 0 | OPT misses 0 | none missed | OPT misses 0 |
| 32K | none missed | none missed | none missed | none missed |
| 64K | none missed | none missed | none missed | none missed |
| 128K | none missed | none missed | none missed | none missed |

Clearing keeps the newest N tool outputs. Median N per budget: 1K: 4, 2K: 6, 4K: 10, 8K: 15, 16K: 15, 32K: 15, 64K: 15, 128K: 15.
N stops at the trajectory's count of tool outputs once everything fits.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 27.5% of re-accessed tokens missed (25.9% of re-accesses), 1,609 tokens held on average.
- Keep everything: no misses, 5,325 tokens held on average.
