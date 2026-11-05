# Compaction notes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When Claude Code compacts a chat, save its summary to AtticDB as a note. Then the next session or another agent can find it.

**Architecture:** A new hook command, `atticdb hook post-compact`, reads Claude Code's `PostCompact` JSON from stdin. It appends 1 turn to the home store through the same client `memory_append` uses, so redaction applies. `atticdb connect claude-code` installs a `PostCompact` group next to the `SessionStart` group. Disconnect and doctor treat both the same way.

**Tech Stack:** Rust, the `atticdb` crate. Integration tests in `crates/atticdb/tests/it/`.

**Spec:** `docs/design/0008-one-engine-local-or-hosted.md`, order of work step 1. Claude Code hook contract: https://code.claude.com/docs/en/hooks.md (PostCompact input: common fields plus `trigger` (`manual` or `auto`) and `compact_summary`. There's no decision control, and stdout isn't read).

## Global Constraints

- A hook never fails a chat. It always exits 0, with nothing on stdout. Errors go to stderr. A 3 s budget and a 4 s hard stop apply, as for `session-start` (`BUDGET`, `HARD_STOP` in hook.rs).
- Writes go through `Client::call("memory_append", ...)`, so `redact.rs` hides secrets first. Never write around it.
- Agent `claude-code`. Session = the repo name `crate::home::repo(cwd)` gives, the same as the session-start block reads.
- Content: `Compaction summary (<trigger>): <summary>`. The summary is cut to 16,000 characters at a char boundary, plus ` (cut)` when cut. `<trigger>` is the input's `trigger`, or `unknown`. When the input has an absolute `transcript_path`, a last line follows: `Full transcript: <path>`.
- Meta: `{"kind": "compact-summary", "trigger": <trigger>, "harness_session": <session_id or null>, "transcript": <transcript_path or null>}`.
- Why the path: a summary keeps what the agent plans next and drops earlier reasons and rejected paths. That is what the 2026-11-05 eval found for Claude's own memory. The transcript is the source. Store a pointer, not a copy, as D94 does for files.
- `--no-hook` skips both hooks. Disconnect removes both, and only AtticDB's groups.
- Windows: exec form with `args`, as the SessionStart group uses.
- Writing: no em dashes, sentences under 20 words. Commits use `git commit -s`, with no Co-authored-by.
- Verify: `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, `cargo clippy --locked --workspace -- -D warnings`, `cargo test --locked -p atticdb`.

## Review Focus

1. A summary holding a token: the stored note has the `[secret:...]` placeholder, never the token. Test: `post_compact_redacts_secrets` (Task 1).
2. Missing or bad input never writes and never prints to stdout. That covers no summary, a blank summary, a relative cwd, non-JSON and an unknown harness. Test: `post_compact_skips_bad_input` (Task 1).
3. No server running: the hook starts one, as session-start does, or gives up by 4 s. Test: `post_compact_starts_the_server` (Task 1).
4. A user's own PostCompact groups survive connect and disconnect. Test: `connect_keeps_user_post_compact_groups` (Task 2).
5. A rerun of connect adds no second PostCompact group. Test: `connect_post_compact_is_idempotent` (Task 2).

---

### Task 1: `atticdb hook post-compact`

**Files:**
- Modify: `crates/atticdb/src/hook.rs`, `crates/atticdb/src/client.rs` (a write method next to `HomeReader::read`, if needed), `crates/atticdb/src/main.rs` (dispatch and usage text)
- Test: `crates/atticdb/tests/it/hook.rs`

**Interfaces:**
- Produces: `pub fn post_compact(harness: Option<&str>, home: Option<&str>) -> u8` in `hook.rs`. It always returns 0. It accepts `--harness claude-code` only. Any other name, or none, logs to stderr and writes nothing.
- Produces: the CLI `atticdb hook post-compact [--harness <name>] [--home <dir>]`. The usage line becomes `atticdb hook <session-start|post-compact> [--harness <name>] [--home <dir>]`.

- [ ] **Step 1: Write the failing tests** in `tests/it/hook.rs`, using the helpers the session-start tests use:
  - `post_compact_saves_the_summary_as_a_note`: cwd is a dir named `myrepo`. Stdin is `{"cwd": .., "trigger": "manual", "session_id": "s1", "transcript_path": "/tmp/t/s1.jsonl", "compact_summary": "We chose struct over JSON for LEDG-7731."}`. Exit 0 and empty stdout. `memory_recent` as agent `claude-code`, session `myrepo`, returns 1 turn. Its content is `Compaction summary (manual): We chose struct over JSON for LEDG-7731.\nFull transcript: /tmp/t/s1.jsonl`. Its meta has `kind` `compact-summary`, `trigger` `manual`, `harness_session` `s1` and `transcript` `/tmp/t/s1.jsonl`. A second case has no `transcript_path`: no transcript line, and `transcript` null.
  - `post_compact_redacts_secrets`: the summary holds `ghp_` plus 36 alphanumerics. The stored content holds `[secret:github-token]` and not the token.
  - `post_compact_skips_bad_input`: for each of these, exit 0, empty stdout and no turn written:
    - no `compact_summary`
    - `"   "`
    - cwd `rel/path`
    - stdin `not json`
    - `--harness codex`
    - no `--harness`
  - `post_compact_cuts_long_summaries`: a summary of 20,000 `a`. The stored summary part is 16,000 `a` followed by ` (cut)`.
  - `post_compact_starts_the_server`: no server running beforehand, and the note is still saved.
  - `session_start_shows_the_compact_note`: after a post-compact, the session-start block for the same cwd contains `Compaction summary (manual)`.
- [ ] **Step 2: Run** `cargo test --locked -p atticdb --test it hook::post_compact`. Expect failures: an unknown hook event and usage errors.
- [ ] **Step 3: Implement** in `hook.rs`.
  - Reuse `repo()`, `Lookup`, `BUDGET`, `HARD_STOP` and the stdin parsing that `block()` uses: the first JSON object, at most `MAX_INPUT`.
  - Write through a client opened as `HomeReader::in_home` opens one, with agent `claude-code`, then `call("memory_append", {"content", "role": "assistant", "meta"})`.
  - Print nothing to stdout.
  - Dispatch in `main.rs` `fn hook`: the event `post-compact`, with the same flag parsing.
- [ ] **Step 4: Run** `cargo test --locked -p atticdb --test it hook`. Expect all to pass. Then run the Global Constraints verify list.
- [ ] **Step 5: Commit** `feat(hook): save Claude Code compaction summaries as notes`.

### Task 2: connect, disconnect and doctor handle the PostCompact hook

**Files:**
- Modify: `crates/atticdb/src/connect/claude.rs`, plus `crates/atticdb/src/doctor.rs` only if its hook check reads SessionStart by name
- Modify: docs that list what `connect claude-code` adds: `README.md`, `docs/` connect pages. Add decision log row `D103` to `docs/design/0002-decision-log.md`.
- Test: `crates/atticdb/tests/it/connect.rs`, and the unit tests in `connect/claude.rs`

**Interfaces:**
- Consumes: the Task 1 CLI, `hook post-compact --harness claude-code`.
- Produces: `connect claude-code` adds a `hooks.PostCompact` group `{"hooks": [{"type": "command", "command": "<quoted exe> hook post-compact --harness claude-code", "timeout": 5}]}`, with no matcher. The home prefix and Windows exec form are the same as for SessionStart. The report line reads `PostCompact hook: added (...)`. Disconnect removes it. Doctor flags a PostCompact handler whose binary is missing or stale, as it does for SessionStart.

- [ ] **Step 1: Write the failing tests:**
  - `connect_adds_post_compact_hook`: after connect, `settings.json` has exactly 1 AtticDB group in `hooks.PostCompact`. The report has `PostCompact hook: added`.
  - `connect_post_compact_is_idempotent`: a second connect leaves 1 group and reports the hook as already there. Use the wording the SessionStart part uses.
  - `connect_keeps_user_post_compact_groups`: a user group in `hooks.PostCompact` survives both connect and disconnect.
  - `disconnect_removes_post_compact_hook`: after disconnect, no AtticDB group is left in `hooks.PostCompact`.
  - `connect_no_hook_skips_post_compact`: `--no-hook` adds neither group.
  - A unit test checks the PostCompact command string, with and without a home.
- [ ] **Step 2: Run** `cargo test --locked -p atticdb --test it connect`. Expect the new tests to fail.
- [ ] **Step 3: Implement.** Generalize the SessionStart group code over the event and its hook args. Don't copy it. These take the event: `hook_args`, `hook_command`, `exec_args`, `hook_group`, `our_group`, `mixed` and `hook_binaries`. So do the add and remove edits. Keep every SessionStart report string and behavior byte for byte.
- [ ] **Step 4: Run** `cargo test --locked -p atticdb`. Expect all to pass. Then run the Global Constraints verify list.
- [ ] **Step 5: Update the docs and add D103.** D103 records:
  - the PostCompact capture, on by default with the hook
  - its opt-out, `--no-hook`
  - that redaction applies, best effort
  - that Codex, Copilot and Gemini have no PostCompact event today
- [ ] **Step 6: Commit** `feat(connect): PostCompact hook for Claude Code`.

## What this plan does not do

- No compaction capture for Codex, Copilot or Gemini.
- No change to what the session-start block shows. A compact note shows as a recent note, cut to 300 characters like any other.
- No hosted-store work. That is 0008's later steps.
