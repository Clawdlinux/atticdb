# Install AtticDB

You are a coding agent. The user asked you to install AtticDB and connect it to the app you're running in.
Follow these steps in order. Run the commands yourself. Show the user each command's output in short.

AtticDB is local, durable memory for coding agents. One binary. It stores memory on this machine and makes no network calls of its own.
Source: https://github.com/Clawdlinux/atticdb. License: MIT or Apache-2.0. Status: early, fine for a laptop, not for production yet.

## Rules

- Never use sudo. Nothing here needs it.
- Don't edit the user's shell files (`~/.zshrc`, `~/.bashrc`, PowerShell profile) without asking first.
- If your sandbox blocks network access, ask the user to approve the install command with network access.
- If a step fails, stop. Show the user the error and the step. Don't guess a workaround.

## 1. Check for an existing install

```sh
atticdb --version
```

If it prints a version, skip to step 3.
On macOS or Linux also try `~/.local/bin/atticdb --version`. If that works, use `~/.local/bin/atticdb` as the command below.

## 2. Install the binary

### macOS or Linux

```sh
curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh
```

The installer puts `atticdb` in `~/.local/bin`. It checks the download against the release's `SHA256SUMS` and never uses sudo.
If it warns that `~/.local/bin` isn't on PATH, use the full path `~/.local/bin/atticdb` for every command below.
Then tell the user to add this line to their shell file, or offer to add it if they agree:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

### Windows x86_64

`install.sh` doesn't run on Windows. In PowerShell:

```powershell
$base = "https://github.com/Clawdlinux/atticdb/releases/latest/download"
$zip  = "atticdb-x86_64-pc-windows-msvc.zip"
$tmp  = Join-Path $env:TEMP "atticdb-install"
New-Item -ItemType Directory -Force $tmp | Out-Null
Invoke-WebRequest "$base/$zip" -OutFile "$tmp\$zip"
Invoke-WebRequest "$base/SHA256SUMS" -OutFile "$tmp\SHA256SUMS"
$want = ((Select-String -Path "$tmp\SHA256SUMS" -SimpleMatch $zip).Line -split '\s+')[0]
$have = (Get-FileHash "$tmp\$zip" -Algorithm SHA256).Hash.ToLower()
if ($want -ne $have) { throw "Checksum mismatch. Nothing was installed." }
$dir = "$env:LOCALAPPDATA\Programs\atticdb"
Expand-Archive "$tmp\$zip" $dir -Force
$env:PATH += ";$dir"
atticdb --version
```

This puts `atticdb.exe` on PATH for this session only. Ask the user before adding `$dir` to their user PATH for good.

### Any system, from source

Only if the user asks for it, or there's no release for this platform. It needs Rust:

```sh
cargo install --git https://github.com/Clawdlinux/atticdb atticdb --locked
```

## 3. Connect the app you're running in

Pick the name of the app you are running in:

| You are running in | Name |
|---|---|
| Claude Code | `claude-code` |
| Codex (CLI or IDE) | `codex` |
| GitHub Copilot CLI | `copilot` |
| GitHub Copilot in VS Code | `vscode` |

Preview the change first. It writes nothing:

```sh
atticdb setup --dry-run --harness <name>
```

Tell the user in 1 or 2 lines what it will change. Then apply it:

```sh
atticdb setup --yes --harness <name>
```

`--yes` is needed because you are not in an interactive terminal.
Setup adds the MCP entry, a chat-start hook and the guide the app reads. It backs up every file it edits.
For Claude Code and Codex, setup uses the app's own command (`claude` or `codex`) for some parts, so run it where that command works.

If setup says it skipped a part and prints a command to run by hand, run that exact command. Then go on to step 4. That's a fix setup gave you, not a guess.

The dry run also lists other apps it found on this machine. Tell the user they can connect those with the same command, as a comma list: `--harness claude-code,codex`.

On Windows, Codex gets only its MCP entry for now. Claude Code needs version 2.1.139 or later for the chat-start hook.

## 4. Check it

```sh
atticdb doctor --harness <name>
```

Every line should say ok or warn. If a check failed, show the user that line. It says what to do.

## 5. Tell the user what happens next

Say it plainly, in 3 short lines at most:

- Memory loads in new chats. Start a new chat in this app to use it. In VS Code, reload the window first.
- In Codex, approve the AtticDB hook once when Codex asks.
- To try it, save a rule and ask for it in a new chat:
  `atticdb fact set --key repo/<repo>/rule --content "Use pnpm, not npm"`, then ask "what are this repo's rules?"

Use the repo's folder name for `<repo>`. Only run that command if the user wants to.

## Undo

`atticdb disconnect <name>` removes exactly what setup added. Full uninstall steps: https://github.com/Clawdlinux/atticdb#uninstall
