# OpenHands trajectories (nebius, SWE-rebench): offline cache headroom on observed traces

Data: [nebius/SWE-rebench-openhands-trajectories](https://huggingface.co/datasets/nebius/SWE-rebench-openhands-trajectories). Trofimova et al., OpenHands Trajectories with Qwen3-Coder-480B-A35B-Instruct, Nebius blog, 2025, CC-BY-4.0. Only aggregate counts appear here.
Sample: 1,000 rows. The 67,074 rows are cut into 100 equal strata, and each gives 1 random run of 10 rows. Seed 87.
Run: `python3 -m research.ctxcache --dataset openhands --sample 1000 --seed 87 --block 10`

These numbers replay fixed traces offline. An agent's next step depends on what its own context held,
so another policy would change the trace itself. They measure offline cache headroom on observed traces,
not what an agent would do under each policy.
Every ratio is over proxy accesses: re-reads plus references inferred from later actions. Re-reads hold 2.0% of the proxy-access tokens here.
95% intervals resample the sampled blocks of 10 rows, 1,000 times.

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 1,000 (skipped: 0 truncated) |
| Accesses | 71,473: 63,059 first, 580 re-reads, 7,834 references |
| Proxy accesses | 8,414, 3,933,099 tokens. Re-reads hold 2.0% of the tokens |
| Compulsory share (first accesses) | 88.2% of accesses, 90.5% of access tokens |
| Object tokens by kind | file 32.4%, output 67.6%, url 0.0% |
| Dead tokens (objects never accessed again) | 92.1% of object tokens. Files 75.7%, outputs 99.9% |
| Reuse distance p50 / p90 / p99 | 318 / 10,458 / 26,069 tokens |
| Peak live tokens per trajectory, p50 / p90, separate percentiles | 1,290 / 4,273. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90, separate percentiles | 36,281 / 54,351. All tool output the trace records |
| Tool output over peak live, per trajectory, p50 / p90 | 28.0x / 85.6x, over the 1,000 trajectories that reuse anything. 0 reuse nothing |
| The 10 trajectories with the most proxy-access tokens | hold 5.2% of them |
| Message tokens, all trajectories | 14,073,243, outside the model (tool output: 37,557,241) |
| Uses of a file with no view of its version | 8,219, not accesses |
| Rows with signs of condensed history | 5 |

## Proxy accesses by subtype

| Subtype | Accesses | Tokens | Share of proxy-access tokens |
|---|---|---|---|
| `reread` | 580 | 77,306 | 2.0% |
| `edit_span` | 2,779 | 1,572,512 | 40.0% |
| `edit` | 1,360 | 384,117 | 9.8% |
| `line_span` | 0 | 0 | 0.0% |
| `line` | 0 | 0 | 0.0% |
| `run` | 3,324 | 1,680,176 | 42.7% |
| `git` | 212 | 121,958 | 3.1% |
| `path` | 159 | 97,030 | 2.5% |

## Missed tokens, % of proxy-access tokens, pooled

All trajectories' missed tokens over all their proxy-access tokens. Long trajectories weigh more.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 67.9 | 68.0 | 67.5 | 69.4 | 70.0 | 43.7 | 50.0 |
| 2K | 55.8 | 56.2 | 56.8 | 58.3 | 58.9 | 25.9 | 30.3 |
| 4K | 44.1 | 44.8 | 47.0 | 48.3 | 47.7 | 9.1 | 13.1 |
| 8K | 29.9 | 30.7 | 37.0 | 34.2 | 32.9 | 0.0 | 1.0 |
| 16K | 8.8 | 11.0 | 10.4 | 13.2 | 10.4 | 0.0 | 0.0 |
| 32K | 0.8 | 1.5 | 0.4 | 1.7 | 0.7 | 0.0 | 0.0 |
| 64K | 0.1 | 0.1 | 0.0 | 0.1 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Missed tokens, mean of per-trajectory %, macro

Each trajectory with proxy accesses weighs the same.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 60.3 | 60.4 | 60.5 | 62.4 | 62.4 | 27.3 | 33.1 |
| 2K | 48.8 | 49.2 | 50.7 | 51.0 | 50.9 | 13.8 | 16.6 |
| 4K | 35.9 | 36.7 | 38.8 | 38.9 | 37.4 | 4.2 | 6.0 |
| 8K | 22.9 | 23.6 | 30.2 | 25.6 | 23.4 | 0.0 | 0.3 |
| 16K | 6.0 | 7.8 | 6.1 | 8.9 | 5.3 | 0.0 | 0.0 |
| 32K | 0.5 | 1.0 | 0.2 | 1.0 | 0.2 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Misses, % of proxy accesses, pooled

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 41.5 | 41.7 | 43.1 | 44.0 | 41.5 | 10.0 | 15.8 |
| 2K | 32.4 | 33.2 | 35.7 | 35.3 | 31.0 | 3.8 | 6.0 |
| 4K | 23.3 | 24.4 | 27.6 | 26.7 | 20.4 | 0.9 | 1.9 |
| 8K | 16.3 | 17.3 | 22.1 | 19.3 | 12.4 | 0.0 | 0.2 |
| 16K | 4.8 | 6.6 | 5.2 | 7.7 | 2.0 | 0.0 | 0.0 |
| 32K | 0.5 | 0.9 | 0.2 | 1.0 | 0.1 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF |
|---|---|---|---|---|---|
| 1K | 1.36x to 1.55x | 1.36x to 1.55x | 1.35x to 1.54x | 1.39x to 1.59x | 1.40x to 1.60x |
| 2K | 1.84x to 2.16x | 1.86x to 2.17x | 1.88x to 2.20x | 1.92x to 2.25x | 1.95x to 2.28x |
| 4K | 3.35x to 4.84x | 3.41x to 4.92x | 3.58x to 5.16x | 3.67x to 5.30x | 3.63x to 5.23x |
| 8K | at least 30.20x | at least 31.04x | at least 37.44x | at least 34.59x | at least 33.25x |
| 16K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 64K | OPT misses 0 | OPT misses 0 | none missed | OPT misses 0 | none missed |
| 128K | none missed | none missed | none missed | none missed | none missed |

## Reference proxy check

Each variant keeps every re-read and only the named references. Cells are pooled missed tokens, % of that variant's proxy-access tokens.

| Variant | Budget | Proxy tokens | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|---|---|
| all proxy accesses | 4K | 3,933,099 | 44.1 | 44.8 | 47.0 | 48.3 | 47.7 | 9.1 | 13.1 |
| all proxy accesses | 8K | 3,933,099 | 29.9 | 30.7 | 37.0 | 34.2 | 32.9 | 0.0 | 1.0 |
| re-reads only | 4K | 77,306 | 58.6 | 58.5 | 59.8 | 58.6 | 50.4 | 12.7 | 12.7 |
| re-reads only | 8K | 77,306 | 38.2 | 38.8 | 38.8 | 38.2 | 24.7 | 0.0 | 0.0 |
| re-reads and edits | 4K | 2,033,935 | 42.9 | 42.9 | 40.5 | 43.5 | 47.1 | 11.5 | 13.0 |
| re-reads and edits | 8K | 2,033,935 | 27.4 | 27.4 | 30.3 | 27.9 | 32.0 | 0.0 | 0.7 |
| re-reads and exact-span edits | 4K | 1,649,818 | 51.7 | 51.7 | 49.0 | 52.3 | 56.5 | 13.9 | 15.6 |
| re-reads and exact-span edits | 8K | 1,649,818 | 33.3 | 33.3 | 36.5 | 33.5 | 38.8 | 0.0 | 0.9 |
| all but run and git references | 4K | 2,130,965 | 43.2 | 43.3 | 41.2 | 44.3 | 47.4 | 12.3 | 14.1 |
| all but run and git references | 8K | 2,130,965 | 27.2 | 27.2 | 29.7 | 28.5 | 31.7 | 0.0 | 0.7 |

## Influence

- LRU at 4K: 44.1% pooled. Leaving out any 1 trajectory gives 43.8% to 44.2%.
- LRU at 8K: 29.9% pooled. Leaving out any 1 trajectory gives 29.6% to 30.0%.
- The 10 trajectories with the most proxy-access tokens hold 5.2% of them.

## Clearing

Clearing (oracle N) keeps the newest N tool outputs. N is tuned on the whole trajectory, and only the average fits the budget.
Median N per budget: 1K: 5, 2K: 8, 4K: 12, 8K: 16, 16K: 40, 32K: 59, 64K: 60, 128K: 60. N stops at the trajectory's count of tool outputs once everything fits.
Clearing (cap) keeps the newest tool outputs that fit the budget at every step. It is causal.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 58.1% of proxy-access tokens missed (40.6% of proxy accesses), 2,765 tokens held on average.
- Keep everything: no misses, 20,121 tokens held on average.
- OpenHands keeps everything unless its condenser runs. Rows with signs of condensation: 5.
