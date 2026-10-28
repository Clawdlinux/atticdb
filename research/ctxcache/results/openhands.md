# OpenHands trajectories (nebius, SWE-rebench)

Data: [nebius/SWE-rebench-openhands-trajectories](https://huggingface.co/datasets/nebius/SWE-rebench-openhands-trajectories). Trofimova et al., OpenHands Trajectories with Qwen3-Coder-480B-A35B-Instruct, Nebius blog, 2025, CC-BY-4.0. Only aggregate counts appear here.
Sample: 1000 rows. The 67,074 rows are cut into 100 equal strata, and each gives 1 random run of 10 rows. Seed 87.
Run: `python3 -m research.ctxcache --dataset openhands --sample 1000 --seed 87 --block 10`

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 1,000 (skipped: 0 truncated) |
| Accesses | 71,453: 63,059 first, 580 re-reads, 7,814 references |
| Compulsory share (first accesses) | 88.3% of accesses, 90.1% of access tokens |
| Object tokens by kind | file 33.0%, output 67.0%, url 0.0% |
| Dead tokens (objects never accessed again) | 91.7% of object tokens. Files 75.0%, outputs 99.9% |
| Reuse distance p50 / p90 / p99 | 388 / 11,173 / 27,458 tokens |
| Peak live tokens per trajectory, p50 / p90 | 1,394 / 4,356. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90 | 38,343 / 57,304. The context with no eviction |
| Message tokens, all trajectories | 14,073,243, outside the model (tool output: 39,914,860) |
| Uses of a file with no view of its version | 8,218, not accesses |
| Rows with signs of condensed history | 5 |

## Missed tokens, % of re-accessed tokens

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 67.3 | 67.4 | 67.0 | 69.1 | 42.1 | 49.2 |
| 2K | 55.0 | 55.2 | 55.8 | 57.5 | 23.7 | 28.5 |
| 4K | 43.7 | 44.4 | 46.5 | 47.5 | 8.3 | 12.2 |
| 8K | 30.0 | 30.8 | 37.0 | 34.1 | 0.0 | 0.9 |
| 16K | 9.7 | 12.0 | 10.7 | 11.9 | 0.0 | 0.0 |
| 32K | 0.9 | 1.6 | 0.5 | 1.1 | 0.0 | 0.0 |
| 64K | 0.1 | 0.1 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Misses, % of re-accesses

| Budget | LRU | FIFO | Clearing | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|
| 1K | 44.7 | 44.9 | 45.6 | 45.4 | 11.0 | 18.0 |
| 2K | 34.9 | 35.5 | 37.6 | 34.5 | 3.8 | 6.6 |
| 4K | 25.5 | 26.4 | 29.5 | 23.9 | 0.9 | 1.9 |
| 8K | 17.7 | 18.8 | 23.7 | 14.9 | 0.0 | 0.2 |
| 16K | 5.5 | 7.4 | 5.9 | 2.8 | 0.0 | 0.0 |
| 32K | 0.6 | 1.0 | 0.2 | 0.1 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing | GDSF |
|---|---|---|---|---|
| 1K | 1.37x to 1.60x | 1.37x to 1.60x | 1.36x to 1.59x | 1.41x to 1.64x |
| 2K | 1.93x to 2.32x | 1.93x to 2.33x | 1.95x to 2.35x | 2.01x to 2.42x |
| 4K | 3.59x to 5.28x | 3.64x to 5.35x | 3.82x to 5.61x | 3.90x to 5.73x |
| 8K | at least 32.28x | at least 33.11x | at least 39.72x | at least 36.70x |
| 16K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 64K | OPT misses 0 | OPT misses 0 | none missed | none missed |
| 128K | none missed | none missed | none missed | none missed |

Clearing keeps the newest N tool outputs. Median N per budget: 1K: 5, 2K: 7, 4K: 11, 8K: 15, 16K: 37, 32K: 58, 64K: 60, 128K: 60.
N stops at the trajectory's count of tool outputs once everything fits.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 55.8% of re-accessed tokens missed (40.6% of re-accesses), 2,947 tokens held on average.
- Keep everything: no misses, 21,367 tokens held on average.
- OpenHands keeps everything unless its condenser runs. Rows with signs of condensation: 5.
