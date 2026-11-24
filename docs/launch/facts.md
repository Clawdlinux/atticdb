# AtticDB launch facts

This sheet holds verified numbers, run commands, wins and losses for launch posts.
It is a source sheet, not ready-to-paste copy. Write Show HN and community posts in your own words.

## What AtticDB is

- Local, open-source memory store for coding agents, written in Rust (`MIT OR Apache-2.0`).
- Stores per-session turn history and keyed facts with revision history on an append-only disk log.
- Replies to a write only after `fdatasync` finishes.
- Wires into Copilot CLI, VS Code, Claude Code, Codex, Gemini CLI, Antigravity and Claude Desktop.
- Zero telemetry and zero model calls per write.
- Status: early. Good for a developer laptop. Not ready for production.

## Install today (v0.3.3)

macOS and Linux with Homebrew:

```sh
brew install clawdlinux/tap/atticdb
atticdb setup
```

macOS and Linux with `curl`:

```sh
curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"   # needed only if the installer said it isn't on PATH
atticdb setup
```

Windows x86_64: download `atticdb-x86_64-pc-windows-msvc.zip` and `SHA256SUMS` from the [latest release](https://github.com/Clawdlinux/atticdb/releases/latest).
The next release (`v0.4.0`) adds `install.ps1`, `crates.io` (`cargo install atticdb --locked`) and the MCP registry entry (`#239`).

## 10-second runnable demo

Run on macOS or Linux with no API keys and no model calls:

```sh
python3 scripts/demo_correction.py
```

Assets: [demo-correction.gif](demo-correction.gif) and [demo-correction.cast](demo-correction.cast).

What happens:
1. Agent A sets `repo/shop/package-manager` to `"Use npm."`.
2. Agent B reads `"Use npm."` at LSN 2.
3. Agent A reads and updates the key to `"Use pnpm, not npm. npm broke the lockfile on 2026-11-02."`.
4. Agent B tries to write `"Use npm. Faster installs."` from its stale read.
5. AtticDB refuses the write, writes nothing, and returns the current value in the conflict reply.
6. `fact_history` returns both revisions in order with their authors.

Exact limit to state: in MCP mode, the refusal updates B's seen revision to the current value (`D107`).
If B sends a second write after seeing the conflict, that retry commits.

## Measured comparisons and benchmarks

### 1. Correction handoff vs MCP reference memory server 2.0.0

Source: [research/correction_eval.py](../../research/correction_eval.py) and [README comparison](../../README.md#memory-comparison) (`#235`).
3 decision changes on fresh stores, repeated 3 times (`9` trials).

| Check | AtticDB | `@modelcontextprotocol/server-memory@2.0.0` |
|---|---:|---:|
| Reader got only the corrected value | 9/9 | 9/9 |
| Reader fetched ordered revisions natively | 9/9 | No native revision tool |
| Stale writer was refused after a concurrent update | 9/9 | 0/9 |

Loss / boundary: `@modelcontextprotocol/server-memory` kept both conflicting edits as observations.
This tests each server's native MCP tools, not custom client-side locking.

### 2. Durable appends vs Redis, Valkey, DiceDB and SQLite

Source: [bench/H2H.md](../../bench/H2H.md) and [bench/h2h.sh](../../bench/h2h.sh).
Azure `Standard_L8s_v3`, Linux ext4, 256-byte appends, `fsync` before every ack, medians of 7 interleaved reps.

| Disk and load | AtticDB | Redis 8.10.2 | Valkey 9.1.2 | DiceDB 1.0.0 | SQLite 3.45.1 WAL FULL |
|---|---:|---:|---:|---:|---:|
| Premium SSD v2, 16 writers (acks/s) | 9,328 | 5,151 | 5,096 | 5,086 | 1,421 |
| Premium SSD v2, 16 writers (max latency) | 4.2 ms | 16.3 ms | 25.8 ms | 11.3 ms | 46,788 ms |
| Premium SSD v2, 64 writers, 5 ms think (p50) | 1.3 ms | 2.9 ms | 2.9 ms | 2.8 ms | 3.9 ms |
| Local NVMe, 16 writers (acks/s) | 48,611 | 44,572 | 42,928 | 44,693 | 24,936 |

Losses to state plainly:
- SQLite runs in process and wins closed-loop `p50` at every writer count (`32 µs` on NVMe, `704 µs` on SSD v2).
- On Premium SSD v2, SQLite has lower `p99` at 4 writers (`2.0 ms` vs `2.4 ms`) and 16 writers (`1.7 ms` vs `2.7 ms`). Its max latency spikes to `9.9 s` and `46.8 s` on WAL checkpoints.
- Past 16 writers on local NVMe, a single writer thread caps AtticDB near `68K` acks/s (`#52`).

### 3. Equal-budget retrieval vs SQLite FTS5

Source: [research/locomo_retrieval/results/2026-11-19-equal-budget.md](../../research/locomo_retrieval/results/2026-11-19-equal-budget.md) (`#235`).
LoCoMo (`1,536` questions, `321` temporal), identical `1,024 B` and `2,048 B` byte budgets.

| Slice and byte ceiling | AtticDB ranked word search | SQLite FTS5 BM25 |
|---|---:|---:|
| All 1,536 questions, 1,024 B | 54.1% | 51.9% |
| All 1,536 questions, 2,048 B | 60.6% | 58.7% |
| 321 temporal questions, 1,024 B | 63.2% | 62.0% |
| 321 temporal questions, 2,048 B | 70.7% | 69.2% |

Losses to state plainly:
- SQLite FTS5 was faster (`0.24 to 0.26 ms` p95 in process vs `0.62 to 0.69 ms` p95 for AtticDB over a Unix socket).
- This measures evidence recall on fixed chunks, not end-to-end answer accuracy.
- AtticDB uses word search only. It has no embeddings or semantic recall yet (`#117`).

### 4. Ranked search p95 at 100K turns

Source: [bench/RESULTS.md](../../bench/RESULTS.md) and [bench/ranked_search.py](../../bench/ranked_search.py) (`#236`).
Replacing a full candidate sort with lazy top-k heap pops cut 100K-turn common-query `p95` from `4.031 ms` to `3.060 ms` (`24%` faster).
At `1K` turns there was no meaningful change.

### 5. Stale rule vs active migration study

Source: [research/stale_rule/results/2026-11-15-results.md](../../research/stale_rule/results/2026-11-15-results.md) (`#232`).
`336` completed Claude Code (`sonnet`) runs across `4` synthetic Python projects with identical working trees and `2` git histories.

- `"Existing code wins"` (from arXiv `2610.30571`) got `12/12` stale rules right, and `0/12` migrations right.
- Every wrong migration run still passed the project's test suite.
- A 1-sentence instruction about whether the rule or the code changed last got `24/24` when the rule lived in git.
- For hook-injected memory outside git with neutral commit messages, undated memory scored `0/8` on stale rules. A save date plus that sentence in user `CLAUDE.md` scored `16/16`. In the hook header alone it scored `14/16` (`6/8` stale, `8/8` migration).

Boundaries: simulated SessionStart hook (no live MCP calls), 4 small 3-commit repos, 1 harness and 1 model. The guide update itself is tracked in `#233`.

## Links

- Website: https://atticdb.com
- Repo: https://github.com/Clawdlinux/atticdb
- Homebrew tap: https://github.com/Clawdlinux/homebrew-tap
- Glama listing: https://glama.ai/mcp/servers/Clawdlinux/atticdb
- Feedback and roadmap: https://github.com/Clawdlinux/atticdb/issues/129
- Good first issues: https://github.com/Clawdlinux/atticdb/labels/good%20first%20issue
