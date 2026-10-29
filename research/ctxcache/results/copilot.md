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
| Accesses | 6,519: 6,079 first, 27 re-reads, 413 references |
| Proxy accesses | 440, 345,800 tokens. Re-reads hold 4.7% of the tokens |
| Compulsory share (first accesses) | 93.3% of accesses, 85.9% of access tokens |
| Object tokens by kind | file 27.4%, output 67.4%, url 5.2% |
| Dead tokens (objects never accessed again) | 90.5% of object tokens. Files 66.9%, outputs 100.0% |
| Reuse distance p50 / p90 / p99 | 3,546 / 104,982 / 333,411 tokens |
| Peak live tokens per trajectory, p50 / p90, separate percentiles | 0 / 7,885. The optimum never misses at this budget |
| Tool output tokens per trajectory, p50 / p90, separate percentiles | 9,117 / 421,521. All tool output the trace records |
| Tool output over peak live, per trajectory, p50 / p90 | 74.9x / 82.7x, over the 7 trajectories that reuse anything. 14 reuse nothing |
| The 10 trajectories with the most proxy-access tokens | hold 100.0% of them |
| Message tokens, all trajectories | 507,224, outside the model (tool output: 2,115,933) |
| Uses of a file with no view of its version | 798, not accesses |
| Compactions | 39 |

## Proxy accesses by subtype

| Subtype | Accesses | Tokens | Share of proxy-access tokens |
|---|---|---|---|
| `reread` | 27 | 16,340 | 4.7% |
| `edit_span` | 8 | 6,012 | 1.7% |
| `edit` | 76 | 71,355 | 20.6% |
| `line_span` | 0 | 0 | 0.0% |
| `line` | 0 | 0 | 0.0% |
| `run` | 5 | 4,556 | 1.3% |
| `git` | 14 | 8,419 | 2.4% |
| `path` | 310 | 239,118 | 69.1% |

## Missed tokens, % of proxy-access tokens, pooled

All trajectories' missed tokens over all their proxy-access tokens. Long trajectories weigh more.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 89.9 | 90.2 | 90.4 | 91.9 | 91.7 | 70.9 | 79.6 |
| 2K | 72.8 | 74.9 | 73.7 | 83.0 | 79.7 | 26.9 | 49.2 |
| 4K | 52.9 | 54.8 | 62.5 | 66.3 | 66.9 | 5.4 | 27.2 |
| 8K | 44.1 | 47.0 | 54.4 | 57.9 | 54.6 | 0.3 | 9.4 |
| 16K | 34.6 | 36.6 | 45.2 | 48.1 | 43.0 | 0.0 | 2.7 |
| 32K | 26.6 | 27.1 | 36.3 | 37.7 | 33.2 | 0.0 | 0.0 |
| 64K | 16.5 | 18.0 | 23.2 | 24.3 | 23.0 | 0.0 | 0.0 |
| 128K | 7.1 | 7.8 | 14.2 | 15.4 | 11.7 | 0.0 | 0.0 |

## Missed tokens, mean of per-trajectory %, macro

Each trajectory with proxy accesses weighs the same.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 78.9 | 79.0 | 60.1 | 82.0 | 81.9 | 30.6 | 39.5 |
| 2K | 76.4 | 77.3 | 52.8 | 83.1 | 72.1 | 11.6 | 21.1 |
| 4K | 30.9 | 31.7 | 45.5 | 38.9 | 42.9 | 2.3 | 11.5 |
| 8K | 24.7 | 25.9 | 33.6 | 30.7 | 23.4 | 0.1 | 3.9 |
| 16K | 20.6 | 21.5 | 22.4 | 26.5 | 18.4 | 0.0 | 1.1 |
| 32K | 11.4 | 11.6 | 15.7 | 16.3 | 14.2 | 0.0 | 0.0 |
| 64K | 7.1 | 7.8 | 10.1 | 10.6 | 9.9 | 0.0 | 0.0 |
| 128K | 3.1 | 3.4 | 6.3 | 6.8 | 5.1 | 0.0 | 0.0 |

## Misses, % of proxy accesses, pooled

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|
| 1K | 79.1 | 79.5 | 82.3 | 83.6 | 79.3 | 39.5 | 54.3 |
| 2K | 65.5 | 67.3 | 71.6 | 76.8 | 65.7 | 13.4 | 33.2 |
| 4K | 50.9 | 53.0 | 63.2 | 65.0 | 52.5 | 3.6 | 20.2 |
| 8K | 41.4 | 44.1 | 55.2 | 56.4 | 41.6 | 0.2 | 7.7 |
| 16K | 33.4 | 35.0 | 45.7 | 48.0 | 30.7 | 0.0 | 2.0 |
| 32K | 23.6 | 24.5 | 37.5 | 37.5 | 23.6 | 0.0 | 0.0 |
| 64K | 14.1 | 16.8 | 25.5 | 25.9 | 15.5 | 0.0 | 0.0 |
| 128K | 7.7 | 8.4 | 13.2 | 17.3 | 7.0 | 0.0 | 0.0 |

## Gap to the optimum, in missed tokens

Each cell is the policy's missed tokens over the upper bound, then over the lower bound.
The policy's multiple of the true optimum lies between the 2. 95% intervals are in the CSV.
"OPT misses 0": the upper bound is 0, so the optimum misses nothing. "at least": the lower bound is 0.

| Budget | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF |
|---|---|---|---|---|---|
| 1K | 1.13x to 1.27x | 1.13x to 1.27x | 1.14x to 1.27x | 1.15x to 1.30x | 1.15x to 1.29x |
| 2K | 1.48x to 2.70x | 1.52x to 2.78x | 1.50x to 2.74x | 1.69x to 3.08x | 1.62x to 2.96x |
| 4K | 1.95x to 9.77x | 2.02x to 10.12x | 2.30x to 11.54x | 2.44x to 12.24x | 2.46x to 12.35x |
| 8K | 4.70x to 138.39x | 5.01x to 147.52x | 5.81x to 170.85x | 6.18x to 181.84x | 5.83x to 171.50x |
| 16K | at least 12.99x | at least 13.76x | at least 16.98x | at least 18.06x | at least 16.13x |
| 32K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 64K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |
| 128K | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 | OPT misses 0 |

## Reference proxy check

Each variant keeps every re-read and only the named references. Cells are pooled missed tokens, % of that variant's proxy-access tokens.

| Variant | Budget | Proxy tokens | LRU | FIFO | Clearing (oracle N) | Clearing (cap) | GDSF | PFOO-L (lower) | Belady-Size (upper) |
|---|---|---|---|---|---|---|---|---|---|
| all proxy accesses | 4K | 345,800 | 52.9 | 54.8 | 62.5 | 66.3 | 66.9 | 5.4 | 27.2 |
| all proxy accesses | 8K | 345,800 | 44.1 | 47.0 | 54.4 | 57.9 | 54.6 | 0.3 | 9.4 |
| re-reads only | 4K | 16,340 | 82.7 | 82.7 | 76.3 | 82.7 | 82.6 | 0.0 | 5.0 |
| re-reads only | 8K | 16,340 | 76.1 | 82.6 | 76.1 | 76.1 | 82.6 | 0.0 | 0.0 |
| re-reads and edits | 4K | 93,707 | 72.5 | 72.5 | 65.2 | 70.3 | 82.2 | 8.4 | 40.6 |
| re-reads and edits | 8K | 93,707 | 64.8 | 65.9 | 54.8 | 63.3 | 74.1 | 0.0 | 22.2 |
| re-reads and exact-span edits | 4K | 22,352 | 69.8 | 69.8 | 55.7 | 69.8 | 69.7 | 0.0 | 3.7 |
| re-reads and exact-span edits | 8K | 22,352 | 55.6 | 60.4 | 55.6 | 55.6 | 69.7 | 0.0 | 0.0 |
| all but run and git references | 4K | 332,825 | 54.4 | 56.1 | 63.0 | 67.2 | 69.0 | 5.5 | 27.9 |
| all but run and git references | 8K | 332,825 | 45.3 | 48.3 | 55.1 | 58.8 | 56.0 | 0.3 | 9.5 |

## Influence

- LRU at 4K: 52.9% pooled. Leaving out any 1 trajectory gives 42.6% to 58.9%.
- LRU at 8K: 44.1% pooled. Leaving out any 1 trajectory gives 36.6% to 49.5%.
- The 10 trajectories with the most proxy-access tokens hold 100.0% of them.

## Clearing

Clearing (oracle N) keeps the newest N tool outputs. N is tuned on the whole trajectory, and only the average fits the budget.
Median N per budget: 1K: 4, 2K: 4, 4K: 8, 8K: 15, 16K: 16, 32K: 16, 64K: 16, 128K: 16. N stops at the trajectory's count of tool outputs once everything fits.
Clearing (cap) keeps the newest tool outputs that fit the budget at every step. It is causal.

## Scaffold baselines

- Keep the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`: 72.6% of proxy-access tokens missed (73.4% of proxy accesses), 1,740 tokens held on average.
- Keep everything: no misses, 331,796 tokens held on average.
- Observed: 12 re-reads in the same compaction window (6,695 tokens), 15 after a compaction (9,645 tokens).
