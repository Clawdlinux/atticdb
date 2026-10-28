# First results, 2026-10-29

Aggregate counts only. The model, the policies and the parsing rules are in [../README.md](../README.md).
There are 3 sources: 1,000 trajectories from each nebius set, and our own Copilot CLI logs.
Budgets count tool output tokens only, as characters / 4. 1K is 1,000 tokens.
Each ratio pools all trajectories. Intervals are 95%, from 1,000 bootstrap resamples of instances (sessions for our logs).

## At a glance

| Measure | SWE-agent | OpenHands | Our Copilot logs |
|---|---|---|---|
| Trajectories used | 1,000 (0 truncated, skipped) | 1,000 (0 truncated, skipped) | 21 (2 forks skipped) |
| Accesses | 35,269 | 71,453 | 6,400 |
| Re-reads / references | 6,049 / 11,417 | 580 / 7,814 | 27 / 409 |
| Compulsory share, accesses / tokens | 50.5% / 46.7% | 88.3% / 90.1% | 93.2% / 86.1% |
| Dead tokens: all / files | 56.9% / 46.2% | 91.7% / 75.0% | 90.5% / 67.0% |
| Reuse distance p50 / p90 / p99 | 285 / 1,723 / 5,586 | 388 / 11,173 / 27,458 | 3,499 / 107,083 / 267,568 |
| Peak live tokens per trajectory p50 / p90 | 1,129 / 2,140 | 1,394 / 4,356 | 0 / 8,002 |
| Tool output tokens per trajectory p50 / p90 | 6,589 / 21,315 | 38,343 / 57,304 | 9,278 / 427,863 |

Peak live tokens is the budget at which the optimum never misses. Tool output tokens is what a window holds with no eviction.
More than half of our sessions re-access nothing, so their peak live is 0.

## Missed tokens, % of re-accessed tokens

At 4K:

| Policy | SWE-agent | OpenHands | Our Copilot logs |
|---|---|---|---|
| LRU | 4.4 (3.0 to 6.3) | 43.7 (41.5 to 46.1) | 52.8 (26.5 to 71.7) |
| FIFO | 5.9 (4.5 to 7.9) | 44.4 (42.2 to 46.7) | 54.8 (26.5 to 73.4) |
| Clearing | 9.0 (7.2 to 11.3) | 46.5 (44.1 to 48.9) | 63.3 (55.7 to 73.4) |
| GDSF | 5.8 (4.3 to 7.6) | 47.5 (45.4 to 49.7) | 68.6 (55.0 to 84.3) |
| PFOO-L, lower bound | 0.8 (0.0 to 2.4) | 8.3 (6.3 to 10.5) | 5.5 (0.0 to 11.2) |
| Belady-Size, upper bound | 1.4 (0.4 to 3.1) | 12.2 (9.9 to 14.6) | 27.7 (0.0 to 43.9) |

At 32K:

| Policy | SWE-agent | OpenHands | Our Copilot logs |
|---|---|---|---|
| LRU | 0.0 | 0.9 (0.5 to 1.3) | 27.2 (0.0 to 35.1) |
| FIFO | 0.0 | 1.6 (1.1 to 2.1) | 27.6 (0.0 to 36.1) |
| Clearing | 0.0 | 0.5 (0.2 to 1.0) | 36.9 (0.0 to 50.9) |
| GDSF | 0.0 | 1.1 (0.5 to 2.0) | 35.9 (0.0 to 47.2) |
| PFOO-L, lower bound | 0.0 | 0.0 | 0.0 |
| Belady-Size, upper bound | 0.0 | 0.0 | 0.0 |

At 128K, every policy misses 0.0% on both public sets.
On our logs LRU misses 7.5% (0.0 to 13.1), FIFO 7.9%, Clearing 14.7% and GDSF 13.1%. Both bounds are 0.0.
The per-source files have every budget from 1K to 128K.

## Gap to the optimum

LRU's missed tokens as a multiple of the optimum's. The 2 bounds bracket it.

| Budget | SWE-agent | OpenHands | Our Copilot logs |
|---|---|---|---|
| 2K | 2.64x to 9.37x | 1.93x to 2.32x | 1.45x to 2.64x |
| 4K | 3.15x to 5.55x | 3.59x to 5.28x | 1.90x to 9.65x |
| 8K | at least 34.71x | at least 32.28x | 4.68x to 172.84x |
| 16K | the optimum misses 0, LRU 0.0% | the optimum misses 0, LRU 9.7% | the optimum misses at most 2.6%, LRU 35.6% |

FIFO, Clearing and GDSF are close to LRU or worse. The [per-source files](swe-agent.md) list their gaps.

## Finding

The optimum needs a small part of the context these agents carry. Practical policies at that size miss several times more than it.

- On OpenHands, the optimum never misses in 90% of trajectories with 4,356 tokens of tool output held. At p90 those trajectories carry 57,304 tokens of tool output, 13 times more.
- At 4K, LRU misses 43.7% of re-accessed tokens (41.5 to 46.1). The optimum misses between 8.3% and 12.2%, so LRU misses 3.6 to 5.3 times as much.
- At 8K, LRU misses 30.0% (27.7 to 32.3), against at most 0.9% for the optimum.
- On OpenHands, FIFO, tool-result clearing and GDSF sit within 7 points of LRU at every budget. The gap to the optimum is far larger than the gap between them.
- SWE-agent shows the same shape at smaller budgets. At 2K, LRU misses 16.1% against 1.7% to 6.1%.
- 91.7% of OpenHands object tokens are never accessed again. So the headroom is in predicting reuse, not in a better recency rule.

Caveats, in order of weight:

- **References are a proxy.** Only paths at their current version and URLs count. Error text, identifiers and search results that steer later steps do not. A broader model would make the optimum need more context. The gap could move either way.
- **The check without the proxy agrees.** On re-reads alone, the agent's own repeat reads, OpenHands at 8K: LRU misses 36.4% (27.3 to 45.5) and the optimum 0. SWE-agent at 2K: LRU 22.4%, the optimum 2.6% to 8.4%. See the `-rereads` files.
- **The agent's own content is outside the model.** On OpenHands, 8,218 uses name a file with no view of its version. 8,196 of them are files the agent wrote and never viewed, mostly scripts it then ran.
- **Stored trajectories may differ from what the model saw.** 5 OpenHands rows hit the 100-iteration cap. They have only 63 to 98 assistant messages. Their tool output is 70K to 103K tokens, so the condenser probably ran. Their stored histories still start at the task. The SWE-agent card does not say whether old observations were dropped from each call.
- **The budget holds tool output only.** Message tokens are another 14.1M on OpenHands, against 39.9M of tool output.
- **Clearing is favored.** Its N is tuned per trajectory on average use, and it can exceed the budget at peaks. It still trails LRU at most budgets.
- **Our logs are 21 sessions** from 1 user. 3 long sessions dominate them, so their intervals are wide.

## Scaffold baselines

- SWE-agent's `LastNObservations` shape, keeping the last 5 outputs, misses 27.5% of re-accessed tokens. It holds 1,609 tokens on average. LRU at 2K misses 16.1%.
- OpenHands keeps everything unless its condenser runs: 21,367 tokens held on average, 0 misses.
- Our Copilot sessions made 37 compactions. 12 re-reads came in the same compaction window as the previous access, and 15 after a compaction.

## Files

| File | Contents |
|---|---|
| `<source>.md` | every table for 1 source |
| `<source>.csv` | 1 row per budget and policy: counts, ratios, 95% intervals, and the gap to the bounds |
| `<source>-trace.csv` | the trace measures |
| `<source>-rereads.*` | the same with references dropped (`--drop-refs`) |

In the CSV, `x_opt_min` is the policy's missed tokens over Belady-Size's and `x_opt_max` over PFOO-L's. The multiple of the optimum lies between them.
Rerun with the commands at the top of each `<source>.md`. Our Copilot numbers change as sessions are added.

## Data and license

- [nebius/SWE-agent-trajectories](https://huggingface.co/datasets/nebius/SWE-agent-trajectories), by Nebius, CC-BY-4.0. The card also asks users of the model outputs to follow the Llama 3.1 License.
- [nebius/SWE-rebench-openhands-trajectories](https://huggingface.co/datasets/nebius/SWE-rebench-openhands-trajectories), by Nebius: Trofimova et al., "OpenHands Trajectories with Qwen3-Coder-480B-A35B-Instruct", Nebius blog, 2025. CC-BY-4.0.

These results are aggregate counts derived from both sets. We changed nothing in the data, and no row is stored in this repo.
