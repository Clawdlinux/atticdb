# Offline cache headroom on observed traces, 2026-10-29

Aggregate counts only. The model, the policies and the parsing rules are in [../README.md](../README.md).

These numbers replay fixed traces offline. An agent's next step depends on what its own context held.
So another context policy would change the trace itself, and these numbers are not what an agent would do under each policy.
They measure offline cache headroom: what a policy could save on these exact traces.

Every ratio is over proxy accesses: re-reads plus references inferred from later actions.
On OpenHands, re-reads hold only 2.0% of the proxy-access tokens, so the inferred references drive its numbers.

There are 3 sources: 1,000 trajectories from each nebius set, and our own Copilot CLI logs.
Our logs are descriptive only: 21 sessions from 1 user.
Budgets count tool output tokens only, as characters / 4. 1K is 1,000 tokens.
Intervals are 95%, from 1,000 bootstrap resamples of instances (SWE-agent), sampled blocks of 10 rows (OpenHands) or sessions (our logs).

## At a glance

| Measure | SWE-agent | OpenHands | Our Copilot logs |
|---|---|---|---|
| Trajectories used | 1,000 (0 truncated) | 1,000 (0 truncated) | 21 (2 forks skipped) |
| Accesses | 35,306 | 71,464 | 6,500 |
| Proxy accesses: re-reads / references | 5,936 / 11,454 | 580 / 7,825 | 27 / 413 |
| Re-reads' share of proxy-access tokens | 27.8% | 2.0% | 4.7% |
| Compulsory share, accesses / tokens | 50.7% / 46.9% | 88.2% / 90.5% | 93.2% / 86.0% |
| Dead tokens: all / files | 57.3% / 47.1% | 92.1% / 75.7% | 90.6% / 66.9% |
| Reuse distance p50 / p90 / p99 | 166 / 1,653 / 5,337 | 321 / 10,462 / 26,069 | 3,634 / 108,082 / 337,682 |
| Peak live tokens p50 / p90, separate percentiles | 1,071 / 2,086 | 1,290 / 4,273 | 0 / 7,895 |
| Tool output tokens p50 / p90, separate percentiles | 6,032 / 20,331 | 36,281 / 54,351 | 9,278 / 427,863 |
| Tool output over peak live, per trajectory, p50 / p90 | 5.8x / 17.0x | 28.0x / 85.6x | 76.2x / 83.3x |
| Trajectories that reuse nothing | 13 | 0 | 14 |
| The 10 largest trajectories' share of proxy-access tokens | 8.6% | 5.2% | 100.0% |

Peak live tokens is the budget at which the optimum never misses, under the proxy model. Tool output tokens is the tool-output volume a trace records.
The tool output over peak live row pairs each trajectory with itself.

Proxy-access tokens by subtype:

| Subtype | SWE-agent | OpenHands | Our Copilot logs |
|---|---|---|---|
| `reread` | 27.8% | 2.0% | 4.7% |
| `edit_span` | 0.0% | 40.0% | 1.7% |
| `edit` | 0.0% | 9.8% | 20.6% |
| `line_span` | 55.1% | 0.0% | 0.0% |
| `line` | 10.0% | 0.0% | 0.0% |
| `run` | 6.9% | 42.7% | 1.3% |
| `git` | 0.0% | 3.1% | 2.4% |
| `path` | 0.3% | 2.5% | 69.2% |

## OpenHands at 4K and 8K

Missed tokens, % of proxy-access tokens. Pooled sums all trajectories, so long ones weigh more. Macro weighs each trajectory the same.

| Budget | Policy | Pooled | Macro |
|---|---|---|---|
| 4K | LRU | 44.1 (41.7 to 46.6) | 35.9 (34.0 to 38.0) |
| 4K | FIFO | 44.9 (42.4 to 47.2) | 36.7 (34.8 to 38.8) |
| 4K | Clearing (oracle N) | 47.1 (44.5 to 49.6) | 38.8 (36.9 to 40.8) |
| 4K | Clearing (cap) | 48.3 (45.7 to 51.0) | 38.9 (37.0 to 40.9) |
| 4K | GDSF | 47.7 (45.2 to 50.1) | 37.4 (35.4 to 39.3) |
| 4K | PFOO-L, lower bound | 9.1 (6.7 to 11.6) | 4.2 (3.1 to 5.3) |
| 4K | Belady-Size, upper bound | 13.2 (10.6 to 15.7) | 6.0 (4.9 to 7.3) |
| 8K | LRU | 29.9 (27.6 to 32.4) | 22.9 (21.3 to 24.7) |
| 8K | FIFO | 30.7 (28.5 to 33.2) | 23.6 (22.0 to 25.3) |
| 8K | Clearing (oracle N) | 37.1 (34.4 to 39.9) | 30.2 (28.1 to 32.5) |
| 8K | Clearing (cap) | 34.2 (31.6 to 36.9) | 25.6 (23.8 to 27.5) |
| 8K | GDSF | 32.9 (30.4 to 35.6) | 23.4 (21.5 to 25.4) |
| 8K | PFOO-L, lower bound | 0.0 | 0.0 |
| 8K | Belady-Size, upper bound | 1.0 (0.4 to 1.6) | 0.3 (0.1 to 0.5) |

Leaving out any 1 trajectory moves LRU between 43.9% and 44.2% at 4K, and between 29.6% and 30.1% at 8K.
The 10 largest trajectories hold 5.2% of the proxy-access tokens.

The same, on narrower proxies. Each variant keeps every re-read and only the named references. Pooled %, macro in brackets:

| Variant | Budget | Proxy tokens | LRU | FIFO | Clearing (cap) | GDSF | PFOO-L | Belady-Size |
|---|---|---|---|---|---|---|---|---|
| all proxy accesses | 4K | 3,930,508 | 44.1 (35.9) | 44.9 (36.7) | 48.3 (38.9) | 47.7 (37.4) | 9.1 (4.2) | 13.2 (6.0) |
| re-reads only | 4K | 77,306 | 58.6 (33.4) | 58.5 (33.4) | 58.6 (33.4) | 50.4 (19.6) | 12.7 (0.6) | 12.7 (0.6) |
| re-reads and edits | 4K | 2,031,344 | 42.9 (31.7) | 42.9 (31.8) | 43.6 (31.9) | 47.1 (32.3) | 11.5 (3.8) | 13.1 (4.3) |
| re-reads and exact-span edits | 4K | 1,647,659 | 51.7 (35.9) | 51.7 (36.0) | 52.4 (36.1) | 56.6 (36.1) | 13.9 (4.1) | 15.6 (4.6) |
| all but run and git references | 4K | 2,128,374 | 43.3 (31.5) | 43.4 (31.6) | 44.3 (31.9) | 47.4 (32.6) | 12.3 (4.1) | 14.1 (4.7) |
| all proxy accesses | 8K | 3,930,508 | 29.9 (22.9) | 30.7 (23.6) | 34.2 (25.6) | 32.9 (23.4) | 0.0 (0.0) | 1.0 (0.3) |
| re-reads only | 8K | 77,306 | 38.2 (22.0) | 38.8 (22.4) | 38.2 (22.0) | 24.7 (9.5) | 0.0 (0.0) | 0.0 (0.0) |
| re-reads and edits | 8K | 2,031,344 | 27.4 (17.5) | 27.4 (17.5) | 27.9 (17.6) | 32.1 (17.8) | 0.0 (0.0) | 0.7 (0.1) |
| re-reads and exact-span edits | 8K | 1,647,659 | 33.3 (19.7) | 33.3 (19.7) | 33.5 (19.7) | 38.8 (19.6) | 0.0 (0.0) | 0.9 (0.1) |
| all but run and git references | 8K | 2,128,374 | 27.3 (17.5) | 27.3 (17.5) | 28.5 (17.8) | 31.8 (18.0) | 0.0 (0.0) | 0.7 (0.1) |

At 4K on re-reads alone, both bounds are 12.7%: those tokens sit in objects larger than 4K, which no policy holds.
Re-reads alone are only 580 accesses, so that row is the noisiest.

## SWE-agent at 2K and 4K

SWE-agent needs less context, so its gap shows at smaller budgets. Pooled, with 95% intervals:

| Budget | LRU | FIFO | Clearing (cap) | GDSF | PFOO-L | Belady-Size |
|---|---|---|---|---|---|---|
| 2K | 14.7 (12.1 to 17.4) | 18.3 (15.4 to 21.2) | 28.5 (25.1 to 31.9) | 22.1 (19.0 to 25.4) | 1.8 (0.6 to 3.6) | 6.1 (4.4 to 8.1) |
| 4K | 4.2 (2.7 to 6.3) | 5.7 (4.1 to 7.7) | 10.6 (8.0 to 13.6) | 5.3 (3.7 to 7.3) | 0.9 (0.0 to 2.6) | 1.5 (0.4 to 3.3) |

On re-reads alone at 2K, LRU misses 21.6% against 2.6% to 8.4% for the optimum. [swe-agent.md](swe-agent.md) has every variant.

## 32K and 128K

| Policy | SWE-agent 32K | OpenHands 32K | Copilot 32K | SWE-agent 128K | OpenHands 128K | Copilot 128K |
|---|---|---|---|---|---|---|
| LRU | 0.0 | 0.8 | 26.5 | 0.0 | 0.0 | 7.4 |
| FIFO | 0.0 | 1.5 | 27.3 | 0.0 | 0.0 | 7.8 |
| Clearing (oracle N) | 0.0 | 0.4 | 36.9 | 0.0 | 0.0 | 14.5 |
| Clearing (cap) | 0.0 | 1.7 | 37.8 | 0.0 | 0.0 | 15.3 |
| GDSF | 0.0 | 0.7 | 35.1 | 0.0 | 0.0 | 12.8 |
| PFOO-L to Belady-Size | 0.0 to 0.0 | 0.0 to 0.0 | 0.0 to 0.0 | 0.0 to 0.0 | 0.0 to 0.0 | 0.0 to 0.0 |

On the public sets these budgets hold nearly every trajectory's live set. Our logs are the exception, from 3 long sessions.

## Finding

Under the proxy model, an offline oracle needs a small part of the tool-output volume recorded in these traces. Recency policies at that size miss several times more.

- On OpenHands, the tool-output volume each trajectory records is 28.0x its own peak live tokens at p50, and 85.6x at p90.
- At 4K, LRU misses 44.1% of proxy-access tokens. The optimum misses between 9.1% and 13.2%, so LRU misses 3.3 to 4.8 times as much. Macro, it is 35.9% against 4.2% to 6.0%.
- At 8K, LRU misses 29.9%, against at most 1.0%.
- From 1K to 8K, FIFO, both clearing variants and GDSF miss between 0.4 points less and 7.2 points more than LRU. None comes near the optimum.
- Every narrower proxy shows the same gap, including re-reads alone and exact-span edits.

This is offline-oracle headroom. An oracle that knows every future reference could save it. No recency rule we tried captures it, and no deployable policy is measured here.

Caveats, in order of weight:

- **Replay is not causal.** A policy that dropped a view would change what the agent did next.
- **The proxy drives OpenHands.** Inferred references hold 98% of its proxy-access tokens: `run` 42.7% and `edit_span` 40.0%. The narrower proxies agree, but none is validated yet. `label-sample` writes a sheet for a human to label.
- **The agent's own content is outside the model.** 8,219 OpenHands uses name a file with no view of its version, mostly scripts the agent wrote.
- **Stored trajectories may differ from what the model saw.** 5 OpenHands rows hit the 100-iteration cap with only 63 to 98 assistant messages. The condenser probably ran.
- **Tool output is not the whole context.** The comparisons above count recorded tool output only. Message tokens are another 14.1M on OpenHands, against 37.6M of tool output. Fixed overhead is outside both.
- **Our logs are descriptive only.** 21 sessions from 1 user. 7 reuse anything, and leaving out 1 moves LRU at 4K between 42.6% and 58.8%.

## Changes since the first draft

- Sizes come from the same canonical body as the key, without tool chrome. That cut OpenHands proxy-access tokens by about 10%.
- The earlier "13 times" compared 2 separate p90s. The paired ratio is 28.0x at p50.
- Clearing is split into an oracle-tuned, average-budget variant and a causal hard-cap variant.
- The shell parser keeps quoted operators and comments apart, and `rg` on a directory is an output.
- The shell parser gives operands roles, so names such as `Makefile` count where a command's operands are files.
  It also skips wrapper options with values, as in `timeout -s KILL 30`. SWE-agent moved by at most 0.3 points.
  Every OpenHands miss ratio stayed the same. On the same Copilot logs, no cell moved more than 1.6 points.

## Scaffold baselines

- Keeping the last 5 tool outputs, the shape of SWE-agent's `LastNObservations`, misses 27.7% of SWE-agent's proxy-access tokens. It holds 1,491 tokens on average.
- OpenHands keeps everything unless its condenser runs: 20,120 tokens held on average, 0 misses.
- Our Copilot sessions made 39 compactions. 12 re-reads came in the same compaction window as the previous access, and 15 after a compaction.

## Files

| File | Contents |
|---|---|
| `<source>.md` | every table for 1 source |
| `<source>.csv` | 1 row per budget and policy: pooled and macro ratios, 95% intervals, leave-one-out range, and the gap to the bounds |
| `<source>-sensitivity.csv` | the same policies on the narrower proxies |
| `<source>-trace.csv` | the trace measures |

In the CSV, `x_opt_min` is the policy's missed tokens over Belady-Size's and `x_opt_max` over PFOO-L's. The multiple of the optimum lies between them.
Rerun with the command at the top of each `<source>.md`. Our Copilot numbers change as sessions are added.

## Data and license

- [nebius/SWE-agent-trajectories](https://huggingface.co/datasets/nebius/SWE-agent-trajectories), by Nebius, CC-BY-4.0. The card also asks users of the model outputs to follow the Llama 3.1 License.
- [nebius/SWE-rebench-openhands-trajectories](https://huggingface.co/datasets/nebius/SWE-rebench-openhands-trajectories), by Nebius: Trofimova et al., "OpenHands Trajectories with Qwen3-Coder-480B-A35B-Instruct", Nebius blog, 2025. CC-BY-4.0.

These results are aggregate counts derived from both sets. We changed nothing in the data, and no row is stored in this repo.
