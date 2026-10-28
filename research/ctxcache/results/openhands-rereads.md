# OpenHands trajectories (nebius, SWE-rebench), re-reads only

Data: [nebius/SWE-rebench-openhands-trajectories](https://huggingface.co/datasets/nebius/SWE-rebench-openhands-trajectories). Trofimova et al., OpenHands Trajectories with Qwen3-Coder-480B-A35B-Instruct, Nebius blog, 2025, CC-BY-4.0. Only aggregate counts appear here.
Sample: 1000 rows. The 67,074 rows are cut into 100 equal strata, and each gives 1 random run of 10 rows. Seed 87.
Run: `python3 -m research.ctxcache --dataset openhands --drop-refs --sample 1000 --seed 87 --block 10`

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 1,000 (skipped: 0 truncated) |
| Accesses | 63,639: 63,059 first, 580 re-reads, 0 references |
| Compulsory share (first accesses) | 99.1% of accesses, 99.8% of access tokens |
| Object tokens by kind | file 33.0%, output 67.0%, url 0.0% |
| Dead tokens (objects never accessed again) | 99.8% of object tokens. Files 99.5%, outputs 99.9% |
| Reuse distance p50 / p90 / p99 | 1,066 / 14,504 / 33,847 tokens |
| Peak live tokens per trajectory, p50 / p90 | 0 / 260. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90 | 38,343 / 57,304. The context with no eviction |
| Message tokens, all trajectories | 14,073,243, outside the model (tool output: 39,914,860) |
| Uses of a file with no view of its version | 8,218, not accesses |
| Rows with signs of condensed history | 5 |

## Missed tokens, % of re-accessed tokens

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 75.2 | 75.3 | 74.5 | 74.2 | 13.6 | 14.5 |
| 2K | 66.3 | 66.8 | 67.8 | 63.2 | 10.1 | 10.1 |
| 4K | 53.8 | 53.8 | 53.9 | 48.4 | 10.1 | 10.1 |
| 8K | 36.4 | 36.7 | 36.9 | 29.9 | 0.0 | 0.0 |
| 16K | 14.6 | 14.6 | 10.3 | 4.1 | 0.0 | 0.0 |
| 32K | 1.6 | 2.1 | 0.1 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Misses, % of re-accesses

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 48.6 | 49.0 | 47.8 | 39.3 | 0.9 | 1.2 |
| 2K | 39.0 | 40.0 | 41.4 | 28.6 | 0.3 | 0.3 |
| 4K | 29.8 | 30.0 | 29.8 | 18.1 | 0.3 | 0.3 |
| 8K | 20.0 | 20.5 | 23.1 | 9.3 | 0.0 | 0.0 |
| 16K | 8.8 | 9.0 | 5.3 | 1.0 | 0.0 | 0.0 |
| 32K | 1.0 | 1.2 | 0.2 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing | GDSF |
|---|---|---|---|---|
| 1K | 5.17x to 5.53x | 5.18x to 5.54x | 5.13x to 5.48x | 5.11x to 5.46x |
| 2K | 6.59x to 6.59x | 6.64x to 6.64x | 6.73x to 6.73x | 6.27x to 6.27x |
| 4K | 5.34x to 5.34x | 5.35x to 5.35x | 5.36x to 5.36x | 4.80x to 4.80x |
| 8K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 16K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | none missed |
| 64K | none missed | none missed | none missed | none missed |
| 128K | none missed | none missed | none missed | none missed |

Clearing keeps the newest N tool outputs. Median N per budget: 1K: 5, 2K: 7, 4K: 11, 8K: 14, 16K: 38, 32K: 59, 64K: 60, 128K: 60.
N stops at the trajectory's count of tool outputs once everything fits.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 62.8% of re-accessed tokens missed (46.7% of re-accesses), 3,026 tokens held on average.
- Keep everything: no misses, 20,677 tokens held on average.
- OpenHands keeps everything unless its condenser runs. Rows with signs of condensation: 5.
