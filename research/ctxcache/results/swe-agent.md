# SWE-agent trajectories (nebius): offline cache headroom on observed traces

Data: [nebius/SWE-agent-trajectories](https://huggingface.co/datasets/nebius/SWE-agent-trajectories). Nebius, SWE-agent-trajectories, CC-BY-4.0. Only aggregate counts appear here.
Sample: 1,000 rows. The 80,036 rows are cut into 1000 equal strata, and each gives 1 random row. Seed 87.
Run: `python3 -m research.ctxcache --dataset swe-agent --sample 1000 --seed 87 --sensitivity-budgets 2k,4k,8k`

These numbers replay fixed traces offline. An agent's next step depends on what its own context held,
so another policy would change the trace itself. They measure offline cache headroom on observed traces,
not what an agent would do under each policy.
Every ratio is over proxy accesses: re-reads plus references inferred from later actions. Re-reads hold 27.8% of the proxy-access tokens here.
95% intervals resample instances, 1,000 times.

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 1,000 (skipped: 0 truncated) |
| Accesses | 35,305: 17,803 first, 6,049 re-reads, 11,453 references |
| Proxy accesses | 17,502, 7,818,386 tokens. Re-reads hold 27.8% of the tokens |
| Compulsory share (first accesses) | 50.4% of accesses, 46.8% of access tokens |
| Object tokens by kind | file 74.9%, output 25.0%, url 0.1% |
| Dead tokens (objects never accessed again) | 57.3% of object tokens. Files 47.1%, outputs 87.5% |
| Reuse distance p50 / p90 / p99 | 171 / 1,633 / 5,334 tokens |
| Peak live tokens per trajectory, p50 / p90, separate percentiles | 1,071 / 2,086. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90, separate percentiles | 6,032 / 20,331. The context with no eviction |
| Tool output over peak live, per trajectory, p50 / p90 | 5.8x / 16.9x, over the 987 trajectories that reuse anything. 13 reuse nothing |
| The 10 trajectories with the most proxy-access tokens | hold 8.6% of them |
| Message tokens, all trajectories | 3,940,279, outside the model (tool output: 9,063,846) |
| Uses of a file with no view of its version | 10, not accesses |

## Proxy accesses by subtype

| Subtype | Accesses | Tokens | Share of proxy-access tokens |
|---|---|---|---|
| `reread` | 6,049 | 2,173,044 | 27.8% |
| `edit_span` | 0 | 0 | 0.0% |
| `edit` | 1 | 1,149 | 0.0% |
| `line_span` | 7,013 | 4,304,732 | 55.1% |
| `line` | 1,131 | 780,693 | 10.0% |
| `run` | 3,088 | 536,101 | 6.9% |
| `git` | 0 | 0 | 0.0% |
| `path` | 220 | 22,667 | 0.3% |

## Missed tokens, % of proxy-access tokens, pooled

All trajectories' missed tokens over all their proxy-access tokens. Long trajectories weigh more.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 65.5 | 66.2 | 57.8 | 62.4 | 67.2 | 37.8 | 44.9 |
| 2K | 14.6 | 18.3 | 24.3 | 28.5 | 22.1 | 1.8 | 6.1 |
| 4K | 4.2 | 5.7 | 8.2 | 10.6 | 5.3 | 0.9 | 1.5 |
| 8K | 0.5 | 1.1 | 0.7 | 2.9 | 0.9 | 0.0 | 0.0 |
| 16K | 0.0 | 0.1 | 0.0 | 0.3 | 0.2 | 0.0 | 0.0 |
| 32K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Missed tokens, mean of per-trajectory %, macro

Each trajectory with proxy accesses weighs the same.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 50.2 | 50.6 | 44.3 | 49.5 | 51.0 | 33.2 | 37.1 |
| 2K | 10.9 | 12.5 | 10.2 | 15.2 | 13.9 | 0.7 | 3.2 |
| 4K | 3.3 | 4.1 | 3.1 | 5.5 | 3.9 | 0.1 | 0.5 |
| 8K | 0.6 | 0.9 | 0.3 | 1.4 | 0.6 | 0.0 | 0.0 |
| 16K | 0.0 | 0.0 | 0.0 | 0.1 | 0.1 | 0.0 | 0.0 |
| 32K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Misses, % of proxy accesses, pooled

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 37.3 | 38.1 | 34.8 | 37.4 | 36.6 | 15.4 | 22.2 |
| 2K | 11.3 | 12.7 | 17.3 | 19.5 | 10.5 | 0.4 | 2.6 |
| 4K | 2.9 | 4.6 | 6.8 | 8.6 | 2.1 | 0.1 | 0.3 |
| 8K | 0.4 | 1.2 | 1.2 | 3.1 | 0.3 | 0.0 | 0.0 |
| 16K | 0.0 | 0.2 | 0.0 | 0.4 | 0.0 | 0.0 | 0.0 |
| 32K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 64K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 128K | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF |
|---|---|---|---|---|---|
| 1K | 1.46x to 1.73x | 1.47x to 1.75x | 1.29x to 1.53x | 1.39x to 1.65x | 1.50x to 1.78x |
| 2K | 2.41x to 8.29x | 3.02x to 10.37x | 4.00x to 13.75x | 4.69x to 16.11x | 3.64x to 12.49x |
| 4K | 2.86x to 4.96x | 3.84x to 6.66x | 5.53x to 9.59x | 7.18x to 12.46x | 3.57x to 6.19x |
| 8K | at least 31.49x | at least 75.21x | at least 50.35x | at least 196.85x | at least 62.47x |
| 16K | OPT misses 0 | OPT misses 0 | none missed | OPT misses 0 | OPT misses 0 |
| 32K | none missed | none missed | none missed | none missed | none missed |
| 64K | none missed | none missed | none missed | none missed | none missed |
| 128K | none missed | none missed | none missed | none missed | none missed |

## Reference proxy check

Each variant keeps every re-read and only the named references. Cells are pooled missed tokens, % of that variant's proxy-access tokens.

| Variant | Budget | Proxy tokens | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|---|---|
| all proxy accesses | 2K | 7,818,386 | 14.6 | 18.3 | 24.3 | 28.5 | 22.1 | 1.8 | 6.1 |
| all proxy accesses | 4K | 7,818,386 | 4.2 | 5.7 | 8.2 | 10.6 | 5.3 | 0.9 | 1.5 |
| all proxy accesses | 8K | 7,818,386 | 0.5 | 1.1 | 0.7 | 2.9 | 0.9 | 0.0 | 0.0 |
| re-reads only | 2K | 2,173,044 | 21.6 | 21.8 | 16.5 | 21.6 | 24.1 | 2.6 | 8.4 |
| re-reads only | 4K | 2,173,044 | 8.8 | 9.3 | 4.8 | 8.8 | 9.3 | 0.0 | 1.4 |
| re-reads only | 8K | 2,173,044 | 1.4 | 1.8 | 0.0 | 1.4 | 1.5 | 0.0 | 0.1 |
| re-reads and edits | 2K | 7,259,618 | 14.1 | 18.2 | 24.3 | 28.5 | 21.7 | 1.9 | 6.4 |
| re-reads and edits | 4K | 7,259,618 | 4.3 | 5.4 | 7.7 | 9.9 | 5.4 | 0.9 | 1.6 |
| re-reads and edits | 8K | 7,259,618 | 0.5 | 0.9 | 0.4 | 2.2 | 1.0 | 0.0 | 0.0 |
| re-reads and exact-span edits | 2K | 6,477,776 | 13.2 | 16.7 | 21.9 | 26.7 | 20.3 | 1.1 | 5.2 |
| re-reads and exact-span edits | 4K | 6,477,776 | 3.5 | 4.6 | 6.2 | 8.1 | 4.6 | 0.0 | 0.7 |
| re-reads and exact-span edits | 8K | 6,477,776 | 0.5 | 0.8 | 0.4 | 1.4 | 0.7 | 0.0 | 0.0 |
| all but run and git references | 2K | 7,282,285 | 14.1 | 18.1 | 24.3 | 28.6 | 21.8 | 1.9 | 6.3 |
| all but run and git references | 4K | 7,282,285 | 4.2 | 5.4 | 7.7 | 9.9 | 5.4 | 0.9 | 1.6 |
| all but run and git references | 8K | 7,282,285 | 0.5 | 0.9 | 0.4 | 2.2 | 1.0 | 0.0 | 0.0 |

## Influence

- LRU at 2K: 14.6% pooled. Leaving out any 1 trajectory gives 13.9% to 14.8%.
- LRU at 4K: 4.2% pooled. Leaving out any 1 trajectory gives 3.4% to 4.3%.
- LRU at 8K: 0.5% pooled. Leaving out any 1 trajectory gives 0.4% to 0.5%.
- The 10 trajectories with the most proxy-access tokens hold 8.6% of them.

## Clearing

Clearing (oracle N) keeps the newest N tool outputs. N is tuned on the whole trajectory, and only the average fits the budget.
Median N per budget: 1K: 5, 2K: 6, 4K: 11, 8K: 15, 16K: 15, 32K: 15, 64K: 15, 128K: 15. N stops at the trajectory's count of tool outputs once everything fits.
Clearing (cap) keeps the newest tool outputs that fit the budget at every step. It is causal.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 27.7% of proxy-access tokens missed (25.9% of proxy accesses), 1,492 tokens held on average.
- Keep everything: no misses, 4,829 tokens held on average.
