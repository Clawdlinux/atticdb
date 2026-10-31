AtticDB runs on Windows x86_64, from [#137](https://github.com/Clawdlinux/atticdb/issues/137). D92 in the [decision log](https://github.com/Clawdlinux/atticdb/blob/main/docs/design/0002-decision-log.md) gives the reasons.

On Windows, Codex gets only its MCP entry for now ([#142](https://github.com/Clawdlinux/atticdb/issues/142)). Claude Code 2.1.139 or later is needed for the hook. Durability there is weaker, as Windows has no directory fsync. [Install](docs:install#on-windows) has the PowerShell steps.

## The home and the profile rule

- The home defaults to `%LOCALAPPDATA%\atticdb`. `ATTICDB_HOME` moves it, as on Unix.
- Windows has no mode bits, and AtticDB checks no access lists. It trusts only the user profile, which is private by default.
- So the home, every data dir, every config and every token file must be inside `%LOCALAPPDATA%` or `%USERPROFILE%`.
- That covers `ATTICDB_HOME`, `--data-dir`, `--config`, `--token-file` and a config's `data_dir`, for the clients and for `atticdb serve`.
- AtticDB makes no private file or folder outside the profile, such as `init --config` or a `verify --scratch` copy.
- Links and short names are resolved first. Anything else fails with an error that names the path and D92.

## The server, over loopback TCP with a proof

- There is no Unix socket. A server always listens on loopback TCP, and `--listen none` fails. `--unix-socket` and `unix:///` URLs fail too.
- A server the CLI starts listens on `127.0.0.1:0`, a free port. `LOCK` names that port, as `tcp=127.0.0.1:<port>`.
- Before the CLI sends its token, it sends `GET /healthz` with a random `X-Atticdb-Nonce` header. That request carries no token.
- The server answers with a `proofs` array, 1 entry per tenant. Each is a keyed BLAKE3 hash of the nonce, the instance id and the server's address on that connection. The key is derived from that tenant's token.
- The CLI sends its token only when 1 proof matches its own token and the address it connected to. So a program squatting on the port never gets the token.
- `--url http://...` skips discovery and the proof. The operator chose that endpoint, so the CLI trusts it.
- `atticdb stats` fails with exit 1. Stats span all tenants, so only the Unix socket serves them. `atticdb doctor` checks the server and store instead.

## Durability is weaker than on Unix

- An ack still waits until the write is flushed to disk.
- Windows has no directory fsync. Creating or deleting a file has no documented flush, so a crash may undo 1.
- A rename uses `MOVEFILE_WRITE_THROUGH`, so it returns only once the move is on disk.
- With a per-user disk quota, free space is what the quota leaves. So the free-space floor follows the quota.

## The apps

VS Code, Copilot CLI and Claude Code get every part. Codex gets only its MCP entry, until [#142](https://github.com/Clawdlinux/atticdb/issues/142).

| App | Windows files | What differs |
|---|---|---|
| VS Code | `%APPDATA%\Code\User\mcp.json` | Nothing else. It reads the Copilot hook and instructions below |
| Copilot CLI | `mcp-config.json`, `hooks\atticdb.json` and `instructions\atticdb.instructions.md` in `%USERPROFILE%\.copilot`, or `%COPILOT_HOME%` | The hook's key is `powershell`: `& '<binary>' hook session-start` |
| Claude Code | `%USERPROFILE%\.claude.json` and `%USERPROFILE%\.claude\settings.json` | The hook runs in exec form, below. `claude` may be `claude.exe` or npm's `claude.cmd` |
| Codex | `%USERPROFILE%\.codex\config.toml` | Only the MCP entry. No hook and no `AGENTS.md` guide |

The Copilot hook's binary path is single-quoted for PowerShell, so `$`, `&` and spaces stay as they are. Under `ATTICDB_HOME`, the hook's `env` passes it, as on Unix. A home with `%` is refused, like one with `$`, as Copilot may expand variables there.

Claude Code's hook needs Claude Code 2.1.139 or later, which added exec form. Claude Code starts the binary with `args`, and no shell:

```json
{"hooks": [{"type": "command", "command": "<binary>", "args": ["hook", "session-start", "--harness", "claude-code"], "timeout": 5}]}
```

Under `ATTICDB_HOME`, `args` ends with `"--home", "<path>"`. The MCP entry passes `-e ATTICDB_HOME=<path>`, as on Unix.

A cold server start can take longer than the hook's 3 s budget. Then the first chat after it gets no memory block. The next chat gets it, as the server is up by then.

## What's missing

Not yet: arm64, an `install.ps1`, and Codex's hooks ([#142](https://github.com/Clawdlinux/atticdb/issues/142)). Ask in an issue if you need the others.

Source: [crates/atticdb/README.md](https://github.com/Clawdlinux/atticdb/blob/main/crates/atticdb/README.md) and [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md)
