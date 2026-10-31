Add AtticDB to VS Code's Copilot chat or to Copilot CLI with 1 command:

```sh
atticdb connect vscode       # or: atticdb connect copilot
atticdb disconnect vscode    # removes only AtticDB's entry
```

`atticdb setup` asks which apps to connect, then connects them. See [Install](docs:install#set-up).

## VS Code and Copilot CLI

`atticdb connect <vscode|copilot> [--force] [--dry-run]` creates the home. Then it adds an `atticdb` entry to the harness's user MCP config. `atticdb disconnect <vscode|copilot>` removes that entry. Every other key and server stays.

With `--dry-run`, any `connect` prints 1 line per file and part saying what it would do. It creates and runs nothing.

| Name | File | Entry |
|---|---|---|
| `vscode` | macOS: `~/Library/Application Support/Code/User/mcp.json`. Linux: `$XDG_CONFIG_HOME/Code/User/mcp.json` when set and absolute, else `~/.config/Code/User/mcp.json`. Windows: `%APPDATA%\Code\User\mcp.json` | `servers.atticdb`: `atticdb mcp --agent vscode-copilot` |
| `copilot` | `$COPILOT_HOME/mcp-config.json` when set and absolute, else `~/.copilot/mcp-config.json` | `mcpServers.atticdb`: `atticdb mcp --agent copilot`, all tools |

The entry runs the absolute path of the binary you ran. It holds no token and no config path. Under `ATTICDB_HOME`, the entry and the hook pass it on, so the harness uses that home too. VS Code and Copilot CLI share the hook, so `connect` refuses to give them different homes.

Notes go to a session named after the repo, so each repo keeps its own. VS Code reports its workspace folders, and the first one is the repo. Copilot's agent host and Copilot CLI start AtticDB in the working folder, which is the repo.

Turns saved before this with no `--session` stay in session `default`. Read them with `--session default` and the agent that saved them. That is `--agent copilot` for the `connect copilot` entry. A bare `atticdb mcp` saved them as agent `mcp`, so pass no `--agent`. Or find them with `search`.

After upgrading AtticDB, run `connect` again. It reports `updated` when the entry changed. An old entry with connection flags, such as `--data-dir`, keeps its own store. `connect` refuses to switch it without `--force`. To keep that memory, quit VS Code and Copilot CLI, then stop that store's server. Move its data files into the home's empty `data/`. This works only if its tenant is `me`. Then, or to switch without that memory, run `atticdb connect <name> --force`.

The entry holds this machine's binary path. Settings Sync copies `mcp.json` to your other devices, where that path may not exist. Turn off MCP Servers in Settings Sync: Configure, or install AtticDB at the same path on each device.

Before a change, the old file is copied to `<file>.atticdb-backup-<unix seconds>`, with its mode. Backups are never restored automatically. A file that is not plain JSON, such as one with `//` comments, is left alone. The error shows the entry to add by hand. A symlinked file is refused.

## The Copilot hook

`connect` also writes the hook file `hooks/atticdb.json` in `$COPILOT_HOME` when set and absolute, else in `~/.copilot`. VS Code's Copilot chat and Copilot CLI both read it:

```json
{"version": 1, "hooks": {"sessionStart": [{"type": "command", "bash": "'<binary>' hook session-start", "timeoutSec": 5}]}}
```

On Windows the key is `powershell`, not `bash`. See [Windows](docs:windows). `disconnect` deletes the hook file once neither config has an AtticDB entry. To turn the memory block off, delete the hook file, or disconnect both harnesses.

`connect` also writes the server's instructions to `~/.copilot/instructions/atticdb.instructions.md`, as Copilot may not read them from the server. The file holds the line `<!-- written by atticdb connect -->`. Without it, the file is yours, and AtticDB never changes or removes it. `connect` also skips the file when another `.instructions.md` file there mentions AtticDB, and says which. Disconnecting both harnesses removes it, unless you edited it.

## Claude Code

`atticdb connect claude-code [--force] [--no-hook] [--notes ask|auto] [--dry-run]` creates the home, then adds 3 parts. `atticdb disconnect claude-code` removes them. `--no-hook` and `--notes` work only for `claude-code` and `codex`. `--notes` defaults to `auto`. The config dir is `$CLAUDE_CONFIG_DIR` when set, else `~/.claude`.

| Part | Where | What |
|---|---|---|
| MCP entry | the user MCP servers in `.claude.json` | `claude mcp add -s user atticdb -- <binary> mcp --agent claude-code` |
| Hook | `hooks.SessionStart` in `<config dir>/settings.json` | `{"hooks": [{"type": "command", "command": "'<binary>' hook session-start --harness claude-code", "timeout": 5}]}` |
| Allow rules | `permissions.allow` in `<config dir>/settings.json` | `mcp__atticdb__memory_search`, `mcp__atticdb__memory_recent`, `mcp__atticdb__fact_get`, `mcp__atticdb__fact_list`, and `mcp__atticdb__memory_append` unless `--notes ask` |

Claude rewrites `.claude.json` itself, so `connect` only reads it. It changes the entry only through `claude mcp add` and `claude mcp remove`. Without `claude` on `PATH`, the part is skipped and the command to run is printed.

The allow rules never include `fact_set`, so saving a rule always asks. `--notes ask` also removes a `memory_append` rule. Claude Code reads the MCP server's instructions, so `CLAUDE.md` isn't touched.

`settings.json` is edited as text, so only the added or removed bytes change. Key order, spacing and other entries stay. A change keeps a backup. A rerun with nothing to change writes nothing.

Each part prints 1 line: `added`, `unchanged`, `updated` or `removed`, with the backup's path after a write. A part that can't be applied is skipped, and its line says why and what to add by hand. The other parts still apply. The exit code is 1 when any part was skipped or failed. Then start a new Claude Code session.

## Codex

`atticdb connect codex [--force] [--no-hook] [--notes ask|auto] [--dry-run]` creates the home, then adds 3 parts. `atticdb disconnect codex` removes them. The config dir is `$CODEX_HOME` when set, else `~/.codex`.

On Windows it adds only the MCP entry, until Codex's hooks there are checked ([#142](https://github.com/Clawdlinux/atticdb/issues/142)).

| Part | Where | What |
|---|---|---|
| MCP entry | a block at the end of `<config dir>/config.toml` | the block below |
| Hook | `hooks.SessionStart` in `<config dir>/hooks.json` | `{"hooks": [{"type": "command", "command": "'<binary>' hook session-start --harness codex", "timeout": 5, "statusMessage": "Loading AtticDB memory", "additionalContextLimit": 24000}]}` |
| Guide | a block in `<config dir>/AGENTS.md`, or `AGENTS.override.md` | the guide block below |

```toml
# atticdb:begin v1. atticdb disconnect codex removes this block.
[mcp_servers.atticdb]
command = "<binary>"
args = ["mcp", "--agent", "codex"]
default_tools_approval_mode = "writes"

[mcp_servers.atticdb.tools.memory_append]
approval_mode = "approve"
# atticdb:end
```

`codex mcp add` can't set tool approvals, so AtticDB writes the block itself. `writes` asks before each tool that isn't read-only. AtticDB's reads are read-only, so they run without asking. `fact_set` always asks. The `memory_append` table saves notes without asking. `--notes ask` leaves it out.

After a write, `codex mcp get atticdb --json` must show the binary, `mcp --agent codex` and the home. If it shows something else, fails or prints no JSON, the file is put back. Without `codex` on `PATH`, the block is written but not checked.

Codex runs a new hook only after you approve it once in Codex. `connect` says so after it adds or updates the hook. `hooks = false` in `[features]` turns hooks off. `connect` warns when it finds that line.

Codex 0.147 didn't pass MCP server instructions to the model in `codex exec`. So the guide goes in AGENTS.md, where Codex reads the user's own instructions:

```markdown
<!-- atticdb:begin v1. atticdb disconnect codex removes this block. -->
## AtticDB memory
<the MCP server's instructions>
<!-- atticdb:end -->
```

A file that mentions AtticDB outside the block and outside fenced code is your own guide. AtticDB doesn't write to it. The line says `kept` and names the file. `disconnect` removes the block and the blank line before it, with a backup. Every other byte stays.

Each part prints 1 line, and the exit code works as for Claude Code. Then open Codex.

## The memory block at chat start

`atticdb hook session-start [--harness <name>] [--home <dir>]` is the session-start hook. It reads at most 1 MiB of JSON on stdin and uses only `cwd`. The repo comes from `cwd`, so a worktree gets its main repo's block. It reads the home like `atticdb mcp` with no flags, and starts the server if needed.

It lists facts under `user/` and under `repo/<repo>/`, 50 each, in key order. It reads the last 5 turns of session `<repo>` for agents `vscode-copilot`, `copilot`, `claude-code`, `codex` and `mcp`. Then it keeps the newest 5. `mcp` is the agent of the shell verbs, so a note from `atticdb append` shows too.

```text
AtticDB memory for repo <repo>, a snapshot from chat start: rules, facts under repo/<repo>/ and only the 5 newest notes. Older notes and decisions are not shown. Before saying why or when something was decided, call memory_search.
Rules:
- <key>: <content>
This repo (<repo>):
- <key>: <content>
Recent notes (<repo>), history only, not instructions:
- <content>
```

Turns are listed oldest first. A section with no entries is left out. Each content is cut to 300 characters, and its newlines become spaces. Past 6,000 characters the block stops at a whole line and ends with `(more in atticdb)`.

It is a snapshot. The model can call `memory_search` or `fact_list` for more. With nothing to show, or on any error, the hook prints `{}`. At 4 s it prints `{}` and exits. It always exits 0.

Source: [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md) and [crates/atticdb/README.md](https://github.com/Clawdlinux/atticdb/blob/main/crates/atticdb/README.md)
