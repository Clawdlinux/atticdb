# zerodb setup eval, 2026-10-30

Can an agent use zerodb memory once `zerodb setup` has wired it in? The method is in [../README.md](../README.md).

## What was run

- zerodb `784fd39` (#114) for the first pass and `5a7ac71` (#121) for the second. Both runs used `~/.cargo/bin/zerodb` built from that commit.
- Claude Code 2.1.310 and Codex 0.147.0, each on its default model, logged in only in its eval config dir.
- Command: `python3 -m research.setup_eval run --harness <h> --scenarios S1,S2,S3,S4,B1,B2 --runs <n>`.
- Billed sessions: 33 of the 36 approved.
  - 30 harness runs: [`2026-10-30.csv`](2026-10-30.csv) (12), [`2026-10-30-r2-codex.csv`](2026-10-30-r2-codex.csv) (12) and [`2026-10-30-r2-claude.csv`](2026-10-30-r2-claude.csv) (6).
  - 3 hand-run Codex probes to find why Codex used no tools. They aren't in the CSVs.
- Every run: a new `ZERODB_HOME` and fixture repo, `zerodb setup --yes --harness <h>` against the eval config dir, then `zerodb disconnect <h>`.
- Guards held:
  - All 138 child processes of the first pass had `ZERODB_HOME` under their own run root.
  - The real zerodb home's data dir and the top level of `~/.claude*`, `~/.codex` and `~/.copilot` didn't change in either pass. That covers names, sizes and mtimes.
- Every stream parsed (`parse_ok` 30 of 30).

## Eval-only overrides

- Claude Code: `--allowedTools=mcp__zerodb__fact_set`, as nobody can answer a prompt.
- Claude Code: `--permission-mode acceptEdits`, so S4 tests memory, not the permission prompt for the edit.
- Codex: `--dangerously-bypass-hook-trust`, and `-c 'mcp_servers.zerodb.tools.fact_set.approval_mode="approve"'` to approve `fact_set`.
- Codex: `--sandbox workspace-write`, named explicitly. A fresh config has no trust entry, so Codex 0.147 would pick read-only.
- Credential and provider vars were stripped from the env. None were set.

## Claude Code (2 runs per cell)

| Scenario | Pass | zerodb calls, median | Input tokens, median | Output tokens, median | Turns, median | Wall s, median |
|---|---|---|---|---|---|---|
| S1 recall | 2/2 | 0 | 22,738 | 80 | 1 | 3.8 |
| S2 search | 2/2 | 2 | 167,070 | 642 | 7 | 9.2 |
| S3 save | 2/2 | 2 | 239,280 | 782 | 5.5 | 11.5 |
| S4 summary | 2/2 | 3 | 295,719 | 1,106 | 10 | 13.3 |
| B1 baseline for S1 | 0/2 | 1.5 | 158,790 | 613 | 6.5 | 8.3 |
| B2 baseline for S2 | 0/2 | 3 | 145,234 | 866 | 8.5 | 10.6 |

Memory decides every cell. S1 to S4 pass in both runs, and both baselines fail with the same setup and an empty store. In S2 the seeded note is pushed out of the chat-start block by 10 later notes. Claude finds it with `memory_search` both times.

## Codex, before the guide fix (1 run per cell)

| Scenario | Pass | zerodb calls, median | Input tokens, median | Output tokens, median | Turns, median | Wall s, median |
|---|---|---|---|---|---|---|
| S1 recall | 1/1 | 0 | 17,949 | 11 | 1 | 9.1 |
| S2 search | 0/1 | 0 | 18,209 | 19 | 1 | 4.4 |
| S3 save | 0/1 | 0 | 73,728 | 432 | 1 | 12.0 |
| S4 summary | 0/1 | 0 | 112,858 | 852 | 1 | 30.2 |
| B1 baseline for S1 | 0/1 | 0 | 74,375 | 560 | 1 | 24.4 |
| B2 baseline for S2 | 0/1 | 0 | 97,006 | 598 | 1 | 18.3 |

Codex made 0 zerodb tool calls in all 6 runs. It passed S1 from the chat-start block alone. In S3 it wrote the rule into the repo's `AGENTS.md`, not into memory.

3 probes found why:
- Codex saw all 6 zerodb tools and called them correctly when told to.
- Asked to quote any zerodb MCP server instructions it had received, it answered none. It had only the chat-start block.

So `codex exec` didn't pass the MCP server's instructions to the model, and #121 writes the guide into Codex's global `AGENTS.md`.

## Codex, after the guide fix (2 runs per cell)

| Scenario | Pass | zerodb calls, median | Input tokens, median | Output tokens, median | Turns, median | Wall s, median |
|---|---|---|---|---|---|---|
| S1 recall | 2/2 | 0 | 18,750 | 13 | 1 | 6.0 |
| S2 search | 0/2 | 0 | 18,445 | 18 | 1 | 5.4 |
| S3 save | 2/2 | 3.5 | 109,663 | 544 | 1 | 15.4 |
| S4 summary | 0/2 | 0 | 149,616 | 1,158 | 1 | 54.4 |
| B1 baseline for S1 | 0/2 | 0 | 85,570 | 688 | 1 | 23.8 |
| B2 baseline for S2 | 0/2 | 3 | 121,682 | 608 | 1 | 22.0 |

Turns are 1 per run, because `codex exec` reports 1 `turn.completed` per invocation.

**What the guide fixed:** S3 went from 0 of 1 to 2 of 2. Codex now saves a stated rule with `fact_set`.

**Still open** (#122):
- **S2, 0 of 2.** When a chat-start block is present, Codex answers from it and its priors, and never searches. Both runs made up a reason ("to keep the calculator stateless"). In B2, with an empty store and no block, it called `memory_search` 3 times.
- **S4, 0 of 2.** Codex made the edit but never saved a summary note.

## Takeaways

- Claude Code with `zerodb setup`: memory works as designed for recall, search, save and summary.
- Codex needs the guide in `AGENTS.md`, which #121 adds. It now saves rules. It doesn't yet search for history beyond the block, or save summaries without being asked.
- n is small: 1 or 2 runs per cell. These are behavior checks, not rates.
