# Connect Gemini CLI, Antigravity and Claude Desktop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `atticdb connect|disconnect|doctor|setup` handle Gemini CLI, Antigravity and Claude Desktop. Today they're wired by hand.

**Architecture:** 3 new `Harness` variants. Antigravity and Claude Desktop get an MCP entry only, through 1 new module for entry-only apps. Gemini CLI gets an MCP entry, a SessionStart hook and a guide block in `GEMINI.md`. The guide reuses Codex's guide code, generalized over the app. Their agent names join `Harness::ALL`, so their notes show at chat start in every app.

**Tech Stack:** Rust, `crates/atticdb`. Integration tests in `crates/atticdb/tests/it/` (`connect.rs`, `setup.rs`, `hook.rs`), with temp homes and stub programs.

**Spec:** issue #191. Contracts:
- Gemini CLI hooks: https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md. `SessionStart` reads `hookSpecificOutput.additionalContext`. Hook input carries `cwd`. `timeout` is in milliseconds.
- Gemini settings: `~/.gemini/settings.json`, keys `mcpServers` and `hooks`. An MCP server's `"trust": true` skips the confirmation before each tool call.
- Antigravity: `~/.gemini/antigravity/mcp_config.json`, key `mcpServers`.
- Claude Desktop: `claude_desktop_config.json`, key `mcpServers`, in `~/Library/Application Support/Claude/` on macOS and `%APPDATA%\Claude\` on Windows. There's no official Linux app.

## Global Constraints

- Names: `gemini`, `antigravity`, `claude-desktop`. Each is both the `--harness` value and the `--agent` name.
- Entries:
  - Gemini: `{"command": <exe>, "args": ["mcp", "--agent", "gemini"], "trust": true}`. Leave out `trust` when `--notes ask`.
  - Antigravity and Claude Desktop: `{"command": <exe>, "args": ["mcp", "--agent", <name>]}`.
  - Under `ATTICDB_HOME`, every entry gets `"env": {"ATTICDB_HOME": <home>}`, as `with_home` does today.
- Gemini hook: a group in `hooks.SessionStart`, `{"hooks": [{"type": "command", "name": "atticdb", "command": "<quoted exe> hook session-start --harness gemini", "timeout": 5000}]}`. The home prefix works as in Claude Code's hook.
- `atticdb hook session-start --harness gemini` prints the `hookSpecificOutput` shape, as `claude-code` does.
- Gemini guide: the MCP server's instructions, as a marker block in `~/.gemini/GEMINI.md`. Markers name `atticdb disconnect gemini`. Use the same rule as Codex: a file that mentions AtticDB outside the block is the user's own, and is left alone.
- File edits go through `jsonedit`, so only the needed bytes change. A file that isn't strict JSON gets a "by hand" line, never a rewrite. Disconnect gives a hand-kept file back byte for byte.
- `--no-hook` skips Gemini's hook. Antigravity and Claude Desktop have no hook.
- Claude Desktop on Linux: `connect` says the app isn't available there. `detect` returns false.
- Antigravity and Claude Desktop read their config only at start. Their connect report ends with `Restart <App> to load it.`
- Existing apps' behavior, output and report strings stay byte-identical.
- Writing: no em dashes, sentences under 20 words. Commits use `git commit -s`, no Co-authored-by.
- Verify: `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`, `cargo clippy --locked --workspace -- -D warnings`, `cargo test --locked -p atticdb`.

## Review Focus

1. A config file that holds comments or trailing commas is never rewritten. Test: `connect_leaves_non_strict_json_alone` (Task 1).
2. A user's own `mcpServers` entries and SessionStart groups survive connect and disconnect, byte for byte. Test: `connect_keeps_user_entries` (Task 1), `gemini_keeps_user_hook_groups` (Task 2).
3. A `GEMINI.md` that already mentions AtticDB outside a block is left alone. Test: `gemini_guide_skips_user_guide` (Task 2).
4. New agents' notes show in every app's chat-start block. Test: `session_start_shows_new_agents_notes` (Task 1).
5. A rerun of connect changes nothing, and reports each part unchanged. Test: `connect_is_idempotent_for_new_apps` (Task 1).

---

### Task 1: Antigravity and Claude Desktop, plus the 3 new names

**Files:**
- Create: `crates/atticdb/src/connect/plain.rs` (entry-only apps)
- Modify: `crates/atticdb/src/connect.rs` (`Harness` enum, `parse`, `name`, `agent`, `file`, `section`, `entry`, `ALL`, `detect`, connect/disconnect dispatch), `crates/atticdb/src/setup.rs`, `crates/atticdb/src/doctor.rs`, `crates/atticdb/src/main.rs` (usage text)
- Test: `crates/atticdb/tests/it/connect.rs`, `crates/atticdb/tests/it/setup.rs`, `crates/atticdb/tests/it/hook.rs`

**Interfaces:**
- Produces: `Harness::{Gemini, Antigravity, ClaudeDesktop}`. `Harness::ALL` has 7 entries, existing order first. `parse`, `name` and `agent` use the names above.
- Produces: `plain::connect(harness, force, mode) -> Report` and `plain::disconnect(harness) -> u8`. Gemini dispatches to a stub that Task 2 replaces. Until then, `connect gemini` reports `not supported yet`.
- `detect`:
  - Antigravity: `~/.gemini/antigravity` exists.
  - Claude Desktop: its config dir exists (macOS, Windows).
  - Gemini: `~/.gemini` exists, or `gemini` is on `PATH`.

- [ ] **Step 1: Write the failing tests**
  - `connect_adds_entry_for_entry_only_apps`: a temp home has an Antigravity and a Claude Desktop config with 1 user server each. Connect adds `atticdb` with the exact entry. The report has `MCP entry: added` and `Restart Antigravity to load it.`, and likewise for Claude Desktop.
  - `connect_keeps_user_entries`: disconnect leaves both files byte for byte as before connect.
  - `connect_is_idempotent_for_new_apps`: a second connect reports unchanged and writes nothing.
  - `connect_leaves_non_strict_json_alone`: a config with a `//` comment is unchanged. The report says to add the entry by hand and shows the entry.
  - `connect_creates_missing_config`: no config file, but the app's dir exists. Connect creates `{"mcpServers": {"atticdb": ...}}`.
  - `session_start_shows_new_agents_notes`: notes appended as agents `gemini`, `antigravity` and `claude-desktop` show in the block for `--harness claude-code`.
  - `setup_detects_new_apps`: setup's plan lists each new app whose dir exists, and none that's missing.
  - `doctor_reports_new_apps`: doctor shows each connected new app's entry as ok, and a stale binary as stale.
  - Unit: `Harness::parse` round-trips all 7 names, and `ALL` order starts with the 4 existing.
- [ ] **Step 2: Run** `cargo test --locked -p atticdb --test it connect setup hook`. Expect the new tests to fail.
- [ ] **Step 3: Implement `plain.rs`.** Reuse the JSON entry machinery `connect_shared` uses for Copilot's file: `add_or_update`, `jsonedit`, `planned`, and the stale-entry and `--force` rules. Don't copy it.
- [ ] **Step 4: Run** the focused tests, then the Global Constraints verify list. All pass.
- [ ] **Step 5: Commit** `feat(connect): Antigravity and Claude Desktop`.

### Task 2: Gemini CLI

**Files:**
- Create: `crates/atticdb/src/connect/gemini.rs`
- Modify: `crates/atticdb/src/connect/codex/guide.rs`. Generalize it over the app (file name, marker text, size limit). Move it to `crates/atticdb/src/connect/guide.rs` if that's needed to share it. Also modify `crates/atticdb/src/connect/claude.rs` only to share the SessionStart group code, and `crates/atticdb/src/hook.rs` to accept `gemini`.
- Test: `crates/atticdb/tests/it/connect.rs`, `crates/atticdb/tests/it/hook.rs`

**Interfaces:**
- Consumes: `Harness::Gemini` and dispatch from Task 1.
- Produces: `gemini::connect(force, hook, notes, mode) -> Report` and `gemini::disconnect() -> u8`. The parts are `MCP entry`, `SessionStart hook` and `atticdb guide`.
- Produces: `hook session-start --harness gemini` prints the `hookSpecificOutput` shape.

- [ ] **Step 1: Write the failing tests**
  - `gemini_connect_adds_entry_hook_and_guide`: in a temp `~/.gemini`, `settings.json` gets the exact entry, with `trust: true`. It also gets the SessionStart group with `timeout` 5000. `GEMINI.md` gets the marker block.
  - `gemini_notes_ask_drops_trust`: with `--notes ask`, the entry has no `trust`.
  - `gemini_no_hook`: `--no-hook` adds no hook group.
  - `gemini_keeps_user_hook_groups`: a user SessionStart group survives connect and disconnect, and the file comes back byte for byte.
  - `gemini_guide_skips_user_guide`: a `GEMINI.md` that mentions AtticDB outside a block is unchanged. The guide part reports skipped.
  - `gemini_disconnect_removes_all_parts`: entry, hook group and guide block all go.
  - `session_start_gemini_shape`: `--harness gemini` prints `{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": ...}}`.
  - Codex's guide tests pass unchanged.
- [ ] **Step 2: Run** the focused tests. Expect failures.
- [ ] **Step 3: Implement.** Generalize Codex's guide and Claude Code's SessionStart group code. Don't copy either. Codex's and Claude Code's output stays byte-identical.
- [ ] **Step 4: Run** the focused tests, then the verify list. All pass.
- [ ] **Step 5: Commit** `feat(connect): Gemini CLI with hook and guide`.

### Task 3: Docs

**Files:**
- Modify: `README.md` and `crates/atticdb/README.md`, wherever they list connect targets or `--harness` values. Add decision log row `D104` to `docs/design/0002-decision-log.md`.

- [ ] **Step 1:** Update the docs. Each new app gets 1 line on what connect writes, and where.
- [ ] **Step 2:** Add D104. It records the 3 apps, the entry-only module, Gemini's `trust` default following `--notes`, and that the 2 desktop apps need a restart.
- [ ] **Step 3: Commit** `docs: connect Gemini CLI, Antigravity and Claude Desktop, D104`.

## What this plan does not do

- No Cursor, Windsurf or Zed. No one has asked yet.
- No change to Antigravity's second config, `~/.gemini/antigravity-ide/mcp_config.json`.
- No Linux Claude Desktop.
- No compaction capture for these apps. None has a compaction event today.
