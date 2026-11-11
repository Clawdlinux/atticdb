# Notes land where the next chat looks. Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An agent's notes land in the session of the repo it works in, so the next chat's start-of-chat block shows them.

**Architecture:**
- 1 sentence in `mcp::INSTRUCTIONS` changes. Every guide `connect` writes is built from that constant (`connect.rs:1154`, `connect/guide.rs:85`).
- The sentence tells agents to pass the repo name as `session`. The stdio and hosted texts become the same, so the hosted session swap goes.

**Tech Stack:** Rust (crates/atticdb). No new dependencies.

**Spec:** this plan's Design section, approved by the owner on 2026-11-11 ("yes go ahead with it").

## Design (approved)

**Problem, with evidence from 2026-11-11:**
- VS Code's agent host runs `atticdb mcp --agent claude-desktop` with cwd `$HOME` (`ps`; `agenthost.log` shows `mcpSource: user`).
- Claude Desktop runs it with cwd `/`.
- Neither gives the adapter roots. So `Client::open` names the session after the cwd folder (`client.rs:429`): `sunny`, or `default`.
- The session-start hook names the session after the chat's cwd repo (`hook.rs:335`).
- So notes saved over MCP go to `sunny` or `default`. The repo's block never shows them. All 15 notes from 2026-11-09 to 11 sit in session `sunny`.

**Fix:**
- In `INSTRUCTIONS`, replace `Leave out the session argument.` with `Pass session=<repo> on memory_append and memory_recent.`
- `<repo>` is already defined earlier in the same text: "the main checkout's folder name, as in the block".
- An explicit `session` argument already beats roots and cwd (`session_arg`), so no code path changes.
- The hosted text no longer needs its own session sentence. Remove `STDIO_SESSION`, `HOSTED_SESSION` and that replace. Hosted still drops `FILE_REF` and `LOCAL_STORE`.

**Approaches considered:**
1. **Agents pass `session` (picked).** It works for every client, with or without roots or a hook.
2. **The hook writes a "current repo" file that `atticdb mcp` reads.** Rejected: 2 chats in 2 repos race on 1 file.
3. **Default the session to `default` when cwd isn't in a git repo.** Rejected: notes still miss the repo's block, and the hook would break for non-repo folders.

**Not in scope:**
- Agent labels. VS Code's agent host still writes as `claude-desktop`. That's cosmetic, since the block reads every agent's notes. It gets a follow-up issue.
- Moving old notes out of `sunny`. The tape is append-only and forget can't move turns. Old notes stay findable with `memory_search`.

## Global Constraints

- `INSTRUCTIONS` and the hosted text stay at most 275 words each. The budget in `instructions_stay_within_word_budget` (`tests/it/mcp.rs:1515-1523`) goes from 270 to 275. Ruled: the new sentence is 2 words over 270, and no rule gets dropped to make room.
- The new sentence is exact: `Pass session=<repo> on memory_append and memory_recent.`
- Every other sentence of `INSTRUCTIONS` stays byte-identical.
- No change to roots handling, `Client::open`, the hook or `home::repo`.
- Prose: no em dashes, sentences under 20 words. Commits: `git commit -s`, no Co-authored-by.

## Review Focus

1. **A client that still omits `session`** still gets roots, then cwd, as today. The existing stdio tests cover it.
2. **Hosted text:** it says the new sentence once, and has none of `Leave out the session argument.`, `file_ref`, `atticdb forget` or `run the atticdb command`.
3. **The guides `connect` writes** contain the new sentence. Copilot instructions, `AGENTS.md`, `GEMINI.md` and Claude's guide all come from `INSTRUCTIONS`.

---

### Task 1: The session sentence

**Files:**
- Modify: `crates/atticdb/src/mcp.rs`: `INSTRUCTIONS` (~342), and `STDIO_SESSION`, `HOSTED_SESSION`, `hosted_instructions` (~28-41, ~105-112)
- Modify: `crates/atticdb/tests/it/mcp.rs`: budget 270 to 275, plus the hosted test asserts (~1515-1570)
- Modify: tests that pin guide text containing `Leave out the session argument.`. Find them with `grep -rn "Leave out the session" crates/atticdb/tests crates/atticdb/src`
- Modify: `docs/design/0002-decision-log.md`, new row D110
- Modify: `docs/design/0010-hosted-mcp-endpoint.md`, "Default session name" section, 1 line saying stdio now uses the same sentence

- [ ] **Step 1: Write the failing tests.**
  - `INSTRUCTIONS` contains `Pass session=<repo> on memory_append and memory_recent.` and not `Leave out the session argument.`
  - `hosted_instructions()` contains the new sentence exactly once, and none of the 4 removed phrases (Review Focus 2).
  - Both texts are at most 275 words.
  - A guide `connect` writes, such as the `connect copilot` instructions file in an existing connect test, contains the new sentence.
- [ ] **Step 2: Run, expect FAIL:** `timeout 900 cargo test -p atticdb --test it mcp::`
- [ ] **Step 3: Implement.** Make the 1-sentence replacement in `INSTRUCTIONS`. Delete `STDIO_SESSION`, `HOSTED_SESSION` and their replace. Update any test that pins the old sentence.
- [ ] **Step 4: Add D110:** "Agents pass `session=<repo>` on `memory_append` and `memory_recent`. Hosts such as VS Code's agent host and Claude Desktop start `atticdb mcp` outside the repo with no roots, so the cwd default sent notes where the start-of-chat block never looks. The stdio and hosted instructions now say the same thing."
- [ ] **Step 5: Full checks, expect PASS.**
  - Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && timeout 1500 cargo test --workspace`
  - The known flaky test (#201) may fail once. Rerun it alone.
  - Then: `PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 PYTHONDONTWRITEBYTECODE=1 python3 -m pytest research -q`
- [ ] **Step 6: Commit:** `git commit -s -m "fix(mcp): agents pass session=<repo>, so notes reach the repo's block"`
