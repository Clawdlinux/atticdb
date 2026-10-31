Pick your system and your apps. The installer below shows exactly what to run.

## What the installer does

```sh
curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"   # needed only if the installer said it isn't on PATH
atticdb setup --dry-run   # shows what it would change, writes nothing
atticdb setup             # asks which apps, then wires them
```

The installer puts `atticdb` in `~/.local/bin`. It checks the download against the release's `SHA256SUMS`. It never uses sudo or edits your shell files.

Releases ship monthly, and the latest is the default. Set `ATTICDB_VERSION=0.1.0` to pin a release. Set `ATTICDB_INSTALL_DIR` to install elsewhere, or `ATTICDB_DOWNLOAD_BASE` for a mirror.

## On Windows

On Windows x86_64, download `atticdb-x86_64-pc-windows-msvc.zip` and `SHA256SUMS` from the [latest release](https://github.com/Clawdlinux/atticdb/releases/latest). Check the zip, then put `atticdb.exe` in a folder on your PATH. In PowerShell:

```powershell
(Get-FileHash .\atticdb-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()   # must match SHA256SUMS
Expand-Archive .\atticdb-x86_64-pc-windows-msvc.zip "$env:LOCALAPPDATA\Programs\atticdb"
$env:PATH += ";$env:LOCALAPPDATA\Programs\atticdb"   # this session only. Add it to your user PATH too
atticdb setup
```

`install.sh` doesn't run on Windows. [Windows](docs:windows) lists what differs there.

## From source

With Rust installed:

```sh
cargo install --git https://github.com/Clawdlinux/atticdb atticdb --locked   # puts atticdb in ~/.cargo/bin
```

`connect` stores the installed binary's path in the entry. An entry that points into a checkout's `target/` breaks on `cargo clean`.

## Set up

```sh
atticdb setup             # finds your apps, asks 3 questions, shows the plan, then connects them
atticdb setup --dry-run   # prints the plan and writes nothing
atticdb setup --help      # the flags
```

`setup` finds Copilot CLI, VS Code, Claude Code and Codex. It adds the MCP entry, a chat-start hook and the guide each app reads. Every edited file is backed up first. `atticdb disconnect <app>` removes exactly what it added. Codex runs a new hook only after you approve it once in Codex.

Nothing runs to find the apps. An app counts as found when its program is on `PATH`, or its config exists. For VS Code, Claude Code and Codex, the config is their config dir. For Copilot CLI, it is `mcp-config.json` or `config.json` in `~/.copilot`.

It asks which apps to connect. Empty answers take the defaults in brackets. For Claude Code and Codex, it also asks 2 things. Should memory load at the start of each chat? Should notes be saved without asking? Saving a rule always asks. VS Code and Copilot CLI always get the hook. They ask before every write until you allow the tool.

The plan has 1 line per file and part, such as `~/.claude/settings.json: add the SessionStart hook`. It comes from the code that writes, run in a mode that writes nothing. Then it asks `Apply? [y/N]`. It connects Copilot CLI and VS Code first, then Claude Code, then Codex. An app that fails doesn't stop the others.

Then it checks each app. A new plan must find every part in place. The hook must print JSON within 5 s. The store must answer `fact_list`. For Codex, `codex mcp get atticdb --json` must show the entry. Last, it prints what to do next.

### Flags

Flags replace the questions. `--harness <list>` takes a comma list of app names. The names are `copilot`, `vscode`, `claude-code` and `codex`. `--no-hook` drops the hook. `--notes ask|auto` picks how notes are saved. `--yes` takes every default and doesn't ask `Apply?`.

Without a terminal on stdin, it needs `--yes` or `--dry-run`. Else it exits 2. `atticdb connect <name> --dry-run` prints the plan for 1 app.

| Exit code | Meaning |
|---|---|
| 0 | Every chosen app connected, with no skipped part, and passed its checks. Also `Apply?` answered no |
| 1 | A part was skipped or failed, a check failed, or no app was found |
| 2 | A usage error, no terminal without `--yes`, or no answer. Also an app that stopped with exit 2 |

## Check the install

```sh
atticdb doctor                    # checks every app it finds
atticdb doctor --harness codex    # checks only these apps, a comma list as in setup
```

It checks the install and writes nothing. Each check prints 1 line: `ok`, or the problem and what to do. They run in this order.

- **atticdb:** the version and the path of this binary.
- **Home:** the path, and whether `ATTICDB_HOME` set it. A missing home says to run `atticdb setup`. Doctor never creates the home, its config, a token or `data/`.
- **Free space:** the bytes free on the home's disk, against the store's floor. The floor is the smaller of 1 GiB and 5% of the disk. Below the floor, writes stop, so the check fails. Below 2x the floor, it warns.
- **Filesystem:** it warns on NFS, SMB, CIFS or FUSE. Locks and syncs may not hold there.
- **Synced folder:** it warns when the home is in iCloud Drive or in `~/Library/CloudStorage`. It also warns in a `Dropbox`, `OneDrive` or `Google Drive` folder.
- **Server and store:** the store must answer `fact_list`. So it may start the home's server.
- **Each app:** the apps it finds, or those in `--harness`. An app that isn't found prints `not found`. That is not a failure. It checks the parts, the binary each entry runs, and the hook. For Codex, it also runs `codex mcp get atticdb --json`.
- **Sandboxes:** a note that agents in a sandbox need read and write access to the home.

The fix for parts and binary is `atticdb setup --harness <name>` or `atticdb connect <name>`.

Copilot CLI's sandbox needs the home in its read and write paths. Else AtticDB can't reach its memory there. In Copilot CLI, run `/sandbox`. On the Filesystem tab, add the home under User-configured paths as Read/Write. That is `~/Library/Application Support/atticdb` on macOS, `~/.local/share/atticdb` on Linux, or your `ATTICDB_HOME`.

| Exit code | Meaning |
|---|---|
| 0 | Every check is ok or warns. Apps that aren't found don't count |
| 1 | A check failed, a part would change, or the home isn't set up |
| 2 | A usage error, such as an unknown app name |

Source: [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md) and [crates/atticdb/README.md](https://github.com/Clawdlinux/atticdb/blob/main/crates/atticdb/README.md)
