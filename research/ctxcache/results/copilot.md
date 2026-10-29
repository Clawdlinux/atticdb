# Our Copilot CLI sessions: offline cache headroom on observed traces

Data: our own Copilot CLI session logs, main agent only. Aggregates only.
Run: `python3 -m research.ctxcache --dataset copilot`

These numbers replay fixed traces offline. An agent's next step depends on what its own context held,
so another policy would change the trace itself. They measure offline cache headroom on observed traces,
not what an agent would do under each policy.
Every ratio is over proxy accesses: re-reads plus references inferred from later actions. Re-reads hold 4.7% of the proxy-access tokens here.
Descriptive only: 21 sessions from 1 user. The intervals resample these sessions. They do not describe other users.

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 21 (skipped: 2 fork) |
| Accesses | 6,500: 6,060 first, 27 re-reads, 413 references |
| Proxy accesses | 440, 346,714 tokens. Re-reads hold 4.7% of the tokens |
| Compulsory share (first accesses) | 93.2% of accesses, 86.0% of access tokens |
| Object tokens by kind | file 27.1%, output 67.8%, url 5.1% |
| Dead tokens (objects never accessed again) | 90.6% of object tokens. Files 66.9%, outputs 100.0% |
| Reuse distance p50 / p90 / p99 | 3,634 / 108,082 / 337,682 tokens |
| Peak live tokens per trajectory, p50 / p90, separate percentiles | 0 / 7,895. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90, separate percentiles | 9,278 / 427,863. All tool output the trace records |
| Tool output over peak live, per trajectory, p50 / p90 | 76.2x / 83.3x, over the 7 trajectories that reuse anything. 14 reuse nothing |
| The 10 trajectories with the most proxy-access tokens | hold 100.0% of them |
| Message tokens, all trajectories | 505,705, outside the model (tool output: 2,147,905) |
| Uses of a file with no view of its version | 795, not accesses |
| Compactions | 39 |

## Proxy accesses by subtype

| Subtype | Accesses | Tokens | Share of proxy-access tokens |
|---|---|---|---|
| `reread` | 27 | 16,340 | 4.7% |
| `edit_span` | 8 | 6,012 | 1.7% |
| `edit` | 76 | 71,396 | 20.6% |
| `line_span` | 0 | 0 | 0.0% |
| `line` | 0 | 0 | 0.0% |
| `run` | 5 | 4,578 | 1.3% |
| `git` | 14 | 8,451 | 2.4% |
| `path` | 310 | 239,937 | 69.2% |

## Missed tokens, % of proxy-access tokens, pooled

All trajectories' missed tokens over all their proxy-access tokens. Long trajectories weigh more.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 90.2 | 90.5 | 90.3 | 92.2 | 91.8 | 70.8 | 79.4 |
| 2K | 72.8 | 74.8 | 74.1 | 83.0 | 78.4 | 26.9 | 49.3 |
| 4K | 52.9 | 54.8 | 62.5 | 66.3 | 68.6 | 5.4 | 27.1 |
| 8K | 44.5 | 47.5 | 54.9 | 57.9 | 54.0 | 0.3 | 9.4 |
| 16K | 35.1 | 36.6 | 45.5 | 49.1 | 44.6 | 0.0 | 2.7 |
| 32K | 26.5 | 27.3 | 36.9 | 37.8 | 35.1 | 0.0 | 0.0 |
| 64K | 16.5 | 18.0 | 25.4 | 24.3 | 24.6 | 0.0 | 0.0 |
| 128K | 7.4 | 7.8 | 14.5 | 15.3 | 12.8 | 0.0 | 0.0 |

## Missed tokens, mean of per-trajectory %, macro

Each trajectory with proxy accesses weighs the same.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 79.0 | 79.2 | 60.1 | 82.1 | 81.9 | 30.6 | 39.4 |
| 2K | 76.4 | 77.3 | 53.0 | 83.1 | 71.5 | 11.6 | 21.1 |
| 4K | 30.9 | 31.7 | 45.5 | 38.9 | 57.8 | 2.3 | 11.5 |
| 8K | 24.9 | 28.3 | 33.8 | 32.9 | 23.2 | 0.1 | 3.9 |
| 16K | 20.8 | 21.4 | 22.5 | 26.9 | 19.1 | 0.0 | 1.1 |
| 32K | 11.4 | 11.7 | 15.9 | 16.3 | 15.1 | 0.0 | 0.0 |
| 64K | 7.1 | 7.8 | 11.1 | 10.6 | 10.5 | 0.0 | 0.0 |
| 128K | 3.2 | 3.4 | 6.4 | 6.8 | 5.5 | 0.0 | 0.0 |

## Misses, % of proxy accesses, pooled

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 79.8 | 80.2 | 82.3 | 84.3 | 80.5 | 39.8 | 54.3 |
| 2K | 65.5 | 67.3 | 72.0 | 76.8 | 64.8 | 13.4 | 33.4 |
| 4K | 50.9 | 53.2 | 63.2 | 65.0 | 54.3 | 3.6 | 20.2 |
| 8K | 42.0 | 44.8 | 55.9 | 56.6 | 41.8 | 0.2 | 7.7 |
| 16K | 33.9 | 35.0 | 45.9 | 49.3 | 32.3 | 0.0 | 2.0 |
| 32K | 23.6 | 24.8 | 37.7 | 37.7 | 24.5 | 0.0 | 0.0 |
| 64K | 14.1 | 16.8 | 27.3 | 25.9 | 16.4 | 0.0 | 0.0 |
| 128K | 8.0 | 8.6 | 13.4 | 17.5 | 7.7 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF |
|---|---|---|---|---|---|
| 1K | 1.14x to 1.27x | 1.14x to 1.28x | 1.14x to 1.27x | 1.16x to 1.30x | 1.16x to 1.30x |
| 2K | 1.48x to 2.71x | 1.52x to 2.78x | 1.50x to 2.76x | 1.68x to 3.09x | 1.59x to 2.91x |
| 4K | 1.95x to 9.76x | 2.02x to 10.12x | 2.31x to 11.54x | 2.45x to 12.24x | 2.53x to 12.66x |
| 8K | 4.76x to 139.55x | 5.08x to 148.77x | 5.88x to 172.23x | 6.19x to 181.58x | 5.78x to 169.43x |
| 16K | at least 13.21x | at least 13.78x | at least 17.15x | at least 18.48x | at least 16.80x |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 64K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 128K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |

## Reference proxy check

Each variant keeps every re-read and only the named references. Cells are pooled missed tokens, % of that variant's proxy-access tokens.

| Variant | Budget | Proxy tokens | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|---|---|
| all proxy accesses | 4K | 346,714 | 52.9 | 54.8 | 62.5 | 66.3 | 68.6 | 5.4 | 27.1 |
| all proxy accesses | 8K | 346,714 | 44.5 | 47.5 | 54.9 | 57.9 | 54.0 | 0.3 | 9.4 |
| re-reads only | 4K | 16,340 | 82.7 | 82.7 | 76.3 | 82.7 | 82.6 | 0.0 | 5.0 |
| re-reads only | 8K | 16,340 | 76.1 | 82.6 | 76.1 | 76.1 | 82.6 | 0.0 | 0.0 |
| re-reads and edits | 4K | 93,748 | 72.5 | 72.5 | 65.2 | 70.3 | 80.6 | 8.4 | 40.6 |
| re-reads and edits | 8K | 93,748 | 64.8 | 65.9 | 56.6 | 63.2 | 72.3 | 0.0 | 22.2 |
| re-reads and exact-span edits | 4K | 22,352 | 69.8 | 69.8 | 55.7 | 69.8 | 69.7 | 0.0 | 3.7 |
| re-reads and exact-span edits | 8K | 22,352 | 55.6 | 60.4 | 55.6 | 55.6 | 69.7 | 0.0 | 0.0 |
| all but run and git references | 4K | 333,685 | 54.4 | 56.0 | 63.0 | 67.1 | 70.0 | 5.5 | 27.9 |
| all but run and git references | 8K | 333,685 | 45.8 | 48.8 | 55.6 | 58.8 | 55.8 | 0.3 | 9.4 |

## Influence

- LRU at 4K: 52.9% pooled. Leaving out any 1 trajectory gives 42.6% to 58.8%.
- LRU at 8K: 44.5% pooled. Leaving out any 1 trajectory gives 36.8% to 49.9%.
- The 10 trajectories with the most proxy-access tokens hold 100.0% of them.

## Clearing

Clearing (oracle N) keeps the newest N tool outputs. N is tuned on the whole trajectory, and only the average fits the budget.
Median N per budget: 1K: 4, 2K: 4, 4K: 8, 8K: 15, 16K: 16, 32K: 16, 64K: 16, 128K: 16. N stops at the trajectory's count of tool outputs once everything fits.
Clearing (cap) keeps the newest tool outputs that fit the budget at every step. It is causal.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 72.6% of proxy-access tokens missed (73.4% of proxy accesses), 1,772 tokens held on average.
- Keep everything: no misses, 336,757 tokens held on average.
- Observed: 12 re-reads in the same compaction window (6,695 tokens), 15 after a compaction (9,645 tokens).
