# Our Copilot CLI sessions: offline cache headroom on observed traces

Data: our own Copilot CLI session logs, main agent only. Aggregates only.
Run: `python3 -m research.ctxcache --dataset copilot`

These numbers replay fixed traces offline. An agent's next step depends on what its own context held,
so another policy would change the trace itself. They measure offline cache headroom on observed traces,
not what an agent would do under each policy.
Every ratio is over proxy accesses: re-reads plus references inferred from later actions. Re-reads hold 4.8% of the proxy-access tokens here.
Descriptive only: 21 sessions from 1 user. The intervals resample these sessions. They do not describe other users.

## Trace

| Measure | Value |
|---|---|
| Trajectories used | 21 (skipped: 2 fork) |
| Accesses | 6,443: 6,007 first, 27 re-reads, 409 references |
| Proxy accesses | 436, 338,714 tokens. Re-reads hold 4.8% of the tokens |
| Compulsory share (first accesses) | 93.2% of accesses, 86.2% of access tokens |
| Object tokens by kind | file 27.3%, output 67.5%, url 5.2% |
| Dead tokens (objects never accessed again) | 90.6% of object tokens. Files 67.0%, outputs 100.0% |
| Reuse distance p50 / p90 / p99 | 3,499 / 107,083 / 267,568 tokens |
| Peak live tokens per trajectory, p50 / p90, separate percentiles | 0 / 8,002. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90, separate percentiles | 9,278 / 427,863. All tool output the trace records |
| Tool output over peak live, per trajectory, p50 / p90 | 72.6x / 83.3x, over the 7 trajectories that reuse anything. 14 reuse nothing |
| The 10 trajectories with the most proxy-access tokens | hold 100.0% of them |
| Message tokens, all trajectories | 501,469, outside the model (tool output: 2,127,542) |
| Uses of a file with no view of its version | 792, not accesses |
| Compactions | 37 |

## Proxy accesses by subtype

| Subtype | Accesses | Tokens | Share of proxy-access tokens |
|---|---|---|---|
| `reread` | 27 | 16,340 | 4.8% |
| `edit_span` | 7 | 5,460 | 1.6% |
| `edit` | 76 | 71,396 | 21.1% |
| `line_span` | 0 | 0 | 0.0% |
| `line` | 0 | 0 | 0.0% |
| `run` | 5 | 4,578 | 1.4% |
| `git` | 14 | 7,436 | 2.2% |
| `path` | 307 | 233,504 | 68.9% |

## Missed tokens, % of proxy-access tokens, pooled

All trajectories' missed tokens over all their proxy-access tokens. Long trajectories weigh more.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 90.0 | 90.3 | 90.2 | 92.2 | 91.7 | 70.2 | 79.0 |
| 2K | 72.5 | 74.6 | 74.0 | 82.7 | 78.2 | 27.5 | 49.8 |
| 4K | 52.8 | 54.8 | 62.1 | 66.0 | 68.6 | 5.5 | 27.7 |
| 8K | 44.6 | 47.6 | 54.4 | 57.4 | 54.0 | 0.3 | 9.5 |
| 16K | 35.6 | 36.8 | 45.1 | 48.7 | 45.6 | 0.0 | 2.6 |
| 32K | 27.2 | 27.6 | 36.9 | 37.8 | 35.9 | 0.0 | 0.0 |
| 64K | 16.8 | 18.4 | 26.1 | 24.9 | 25.1 | 0.0 | 0.0 |
| 128K | 7.5 | 7.9 | 14.7 | 15.6 | 13.1 | 0.0 | 0.0 |

## Missed tokens, mean of per-trajectory %, macro

Each trajectory with proxy accesses weighs the same.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 79.0 | 79.1 | 60.1 | 82.1 | 81.9 | 30.3 | 39.1 |
| 2K | 76.2 | 77.1 | 52.9 | 83.0 | 71.4 | 11.7 | 21.3 |
| 4K | 30.7 | 31.6 | 45.2 | 38.7 | 57.7 | 2.3 | 11.7 |
| 8K | 24.8 | 28.3 | 33.4 | 32.5 | 23.1 | 0.1 | 3.9 |
| 16K | 20.9 | 21.4 | 22.2 | 26.6 | 19.4 | 0.0 | 1.1 |
| 32K | 11.5 | 11.7 | 15.8 | 16.1 | 15.3 | 0.0 | 0.0 |
| 64K | 7.1 | 7.8 | 11.2 | 10.7 | 10.6 | 0.0 | 0.0 |
| 128K | 3.2 | 3.4 | 6.4 | 6.8 | 5.6 | 0.0 | 0.0 |

## Misses, % of proxy accesses, pooled

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 79.4 | 79.8 | 82.3 | 84.4 | 80.0 | 38.3 | 53.2 |
| 2K | 64.9 | 66.7 | 72.2 | 76.8 | 64.2 | 13.3 | 33.3 |
| 4K | 50.7 | 53.0 | 63.3 | 65.1 | 53.9 | 3.7 | 20.4 |
| 8K | 42.0 | 44.7 | 56.0 | 56.7 | 41.5 | 0.2 | 7.6 |
| 16K | 33.9 | 34.9 | 46.1 | 49.5 | 32.3 | 0.0 | 1.6 |
| 32K | 23.9 | 24.8 | 38.3 | 38.3 | 24.5 | 0.0 | 0.0 |
| 64K | 13.8 | 16.7 | 28.0 | 26.6 | 16.3 | 0.0 | 0.0 |
| 128K | 7.6 | 8.3 | 13.1 | 17.2 | 7.8 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF |
|---|---|---|---|---|---|
| 1K | 1.14x to 1.28x | 1.14x to 1.29x | 1.14x to 1.29x | 1.17x to 1.31x | 1.16x to 1.31x |
| 2K | 1.45x to 2.64x | 1.50x to 2.72x | 1.48x to 2.69x | 1.66x to 3.01x | 1.57x to 2.85x |
| 4K | 1.90x to 9.65x | 1.98x to 10.01x | 2.24x to 11.35x | 2.38x to 12.06x | 2.47x to 12.52x |
| 8K | 4.68x to 172.84x | 5.00x to 184.51x | 5.71x to 210.70x | 6.03x to 222.54x | 5.67x to 209.26x |
| 16K | at least 13.43x | at least 13.89x | at least 17.02x | at least 18.38x | at least 17.22x |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 64K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 128K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |

## Reference proxy check

Each variant keeps every re-read and only the named references. Cells are pooled missed tokens, % of that variant's proxy-access tokens.

| Variant | Budget | Proxy tokens | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|---|---|
| all proxy accesses | 4K | 338,714 | 52.8 | 54.8 | 62.1 | 66.0 | 68.6 | 5.5 | 27.7 |
| all proxy accesses | 8K | 338,714 | 44.6 | 47.6 | 54.4 | 57.4 | 54.0 | 0.3 | 9.5 |
| re-reads only | 4K | 16,340 | 82.7 | 82.7 | 76.3 | 82.7 | 82.6 | 0.0 | 5.0 |
| re-reads only | 8K | 16,340 | 76.1 | 82.6 | 76.1 | 76.1 | 82.6 | 0.0 | 0.0 |
| re-reads and edits | 4K | 93,196 | 72.9 | 72.9 | 65.5 | 70.7 | 81.1 | 8.4 | 40.9 |
| re-reads and edits | 8K | 93,196 | 65.1 | 66.3 | 56.9 | 63.6 | 72.8 | 0.0 | 22.3 |
| re-reads and exact-span edits | 4K | 21,800 | 71.6 | 71.6 | 57.2 | 71.6 | 71.5 | 0.0 | 3.8 |
| re-reads and exact-span edits | 8K | 21,800 | 57.0 | 61.9 | 57.0 | 57.0 | 71.5 | 0.0 | 0.0 |
| all but run and git references | 4K | 326,700 | 54.2 | 55.9 | 62.7 | 66.9 | 69.5 | 5.5 | 28.5 |
| all but run and git references | 8K | 326,700 | 45.8 | 48.8 | 55.2 | 58.4 | 55.1 | 0.3 | 9.6 |

## Influence

- LRU at 4K: 52.8% pooled. Leaving out any 1 trajectory gives 42.2% to 58.8%.
- LRU at 8K: 44.6% pooled. Leaving out any 1 trajectory gives 36.7% to 49.8%.
- The 10 trajectories with the most proxy-access tokens hold 100.0% of them.

## Clearing

Clearing (oracle N) keeps the newest N tool outputs. N is tuned on the whole trajectory, and only the average fits the budget.
Median N per budget: 1K: 4, 2K: 4, 4K: 8, 8K: 15, 16K: 16, 32K: 16, 64K: 16, 128K: 16. N stops at the trajectory's count of tool outputs once everything fits.
Clearing (cap) keeps the newest tool outputs that fit the budget at every step. It is causal.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 72.1% of proxy-access tokens missed (73.4% of proxy accesses), 1,771 tokens held on average.
- Keep everything: no misses, 334,358 tokens held on average.
- Observed: 12 re-reads in the same compaction window (6,695 tokens), 15 after a compaction (9,645 tokens).
