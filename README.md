# AtticDB

[![Release](https://img.shields.io/github/v/release/Clawdlinux/atticdb)](https://github.com/Clawdlinux/atticdb/releases/latest)
[![Downloads](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2FClawdlinux%2Fatticdb%2Fbadges%2Fdownloads.json)](https://github.com/Clawdlinux/atticdb/releases)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)](#where-to-run-it)

AtticDB is local, durable memory for coding agents, written in Rust.
It stores turn history per session and keyed facts. A write is acked only after it is synced to disk.
It wires into Copilot CLI, VS Code, Claude Code, Codex, Gemini CLI, Antigravity and Claude Desktop with 1 command.

Status: early. Fine for your laptop. Not for production yet.

## Install

On macOS and Linux:

```sh
curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"   # needed only if the installer said it isn't on PATH
```

The installer puts `atticdb` in `~/.local/bin` and checks the download against the release's `SHA256SUMS`. It never uses sudo or edits your shell files.
Releases ship monthly, and the latest is the default.
Set `ATTICDB_VERSION=0.1.0` to pin a release, `ATTICDB_INSTALL_DIR` to install elsewhere, or `ATTICDB_DOWNLOAD_BASE` for a mirror.

On Windows x86_64, download `atticdb-x86_64-pc-windows-msvc.zip` and `SHA256SUMS` from the [latest release](https://github.com/Clawdlinux/atticdb/releases/latest).
Check the zip, then put `atticdb.exe` in a folder on your PATH. In PowerShell:

```powershell
(Get-FileHash .\atticdb-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()   # must match SHA256SUMS
Expand-Archive .\atticdb-x86_64-pc-windows-msvc.zip "$env:LOCALAPPDATA\Programs\atticdb"
$env:PATH += ";$env:LOCALAPPDATA\Programs\atticdb"   # this session only. Add it to your user PATH too
atticdb setup
```

`install.sh` doesn't run on Windows.

From source, with Rust installed:

```sh
cargo install --git https://github.com/Clawdlinux/atticdb atticdb --locked   # puts atticdb in ~/.cargo/bin
```

The downloads badge counts release archive downloads. Our own install tests are left out. A daily workflow updates it.

## Quick start

```sh
atticdb setup --dry-run   # shows what it would change, writes nothing
atticdb setup             # asks which apps, then wires them
atticdb doctor            # checks the install; it may start the server
```

`setup` finds Copilot CLI, VS Code, Claude Code, Codex, Gemini CLI, Antigravity and Claude Desktop. It adds the MCP entry, a chat-start hook and the guide each app reads. Antigravity and Claude Desktop get only the MCP entry. Restart them to load it. Claude Code also gets a PostCompact hook. It saves each compaction summary as a note, with a pointer to the full transcript. Every edited file is backed up first. `atticdb disconnect <app>` removes exactly what it added. Codex runs a new hook only after you approve it once in Codex. [research/setup_eval/results](research/setup_eval/results/README.md) has the eval results.

`atticdb doctor` prints 1 line per check, with what to do when one fails. [Set up](crates/atticdb/README.md#set-up) and [`atticdb doctor`](crates/atticdb/README.md#atticdb-doctor) give the details.

## Memory tree

AtticDB saves facts under keys that work like folder paths. Use these top-level folders:

| Key | What goes there | Loaded at chat start |
|---|---|---|
| `user/<topic>` | Your preferences and style | Yes, in every repo |
| `repo/<repo>/<area>/<topic>` | This project's rules and decisions | Yes, in that repo |
| `agent/<agent>/<topic>` | Notes for 1 app only | Not yet |
| `team/<team>/` | Reserved for sync later | Don't write here yet |

`<repo>` is the folder name of the repo's main checkout. `<area>` is optional. It helps once a repo has many rules, as in `repo/myrepo/api/retry`.
Your agents save a rule here when you state it in a chat. You can save one yourself:

```sh
atticdb fact set --key user/style --content "Short commit messages."
atticdb fact set --key repo/myrepo/tests/runner --content "Run cargo test before a PR."
```

`atticdb tree` shows how your memory is organized. Each folder gets a fact count and the date of its newest change:

```text
$ atticdb tree
(all keys)  6 facts, newest 2026-11-04
|-- repo/  4 facts, newest 2026-11-04
|   |-- myrepo/  3 facts, newest 2026-11-04
|   `-- web/  1 fact, newest 2026-11-04
`-- user/  2 facts, newest 2026-11-04
    |-- style  2026-11-04
    `-- tone  2026-11-04
Deeper levels show as counts. Pass --depth <n> to see more.
```

With no prefix it shows 2 levels. `atticdb tree repo/myrepo/` shows 1 folder, all levels.
`--depth <n>` sets the levels. `--content` adds each value's first line. `--json` prints 1 JSON line for scripts.
Deleted and forgotten keys never show. [`atticdb tree`](crates/atticdb/README.md#atticdb-tree) has the details.

## Forget and export

Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159).

A forget hides a session, a fact, a file ref or single records. After it, your agents, search, studio and exports stop showing that content.
It doesn't erase anything. The bytes stay in the data folder. Real erase is tracked in [#159](https://github.com/Clawdlinux/atticdb/issues/159).
A forget covers only what was written before it. Notes written later still show.
AtticDB can't undo a forget. No MCP tool forgets, so your agents can't forget through their memory tools.
You forget from the shell or from studio. An agent that runs shell commands could still run `atticdb forget`.
Setup adds no shell allow rule for it, so check the commands your agent asks to run.

### Forget from the shell

```sh
atticdb forget session --agent copilot --session myrepo   # a whole session
atticdb forget key --key repo/myrepo/deploy-rule          # 1 fact
atticdb forget file --path shots/login.png                # 1 file ref. The file may be gone
atticdb forget lsns --lsns 12,15                          # single records, by LSN
```

With no store flags, these use your home, the store `atticdb setup` wired up.
Agents save notes under an agent name, such as `copilot` or `claude-code`, and a session named after the repo.
`atticdb recent --agent copilot --session myrepo` lists that session's turns with their LSNs. Studio shows them too.
Add `--up-to-lsn <n>` to hide only records at or below LSN `n`. Each forget prints 3 lines:

```text
forgot session copilot/myrepo up to lsn 40 (txn 9, lsn 40)
Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159).
Names, keys, LSNs, counts and sizes stay visible. See "What a forget leaves visible" in the README.
```

`atticdb forget` doesn't ask "are you sure?". Typing the command is the only friction.
It isn't an approval step either. Anything that holds your token can call `POST /v1/forget`.
`atticdb dump`, `atticdb stats` and `atticdb verify` never print content. Their counts and sizes still include forgotten records.

The store holds at most 100,000 hidden LSNs, set by the engine option `max_forgotten_lsns`.
Past that, `forget lsns` fails with `resource_exhausted`. Session and key forgets don't count toward it.
The server config doesn't set this option. If you embed the engine and lower it below the LSNs already hidden, open fails until you raise it.
`max_facts` works the same way.

### Forget from studio

Studio is read-only unless you start it with `--allow-forget`:

```sh
atticdb studio --allow-forget
```

The Sessions table then gets a box on each row and a "Forget selected (N)" button. It takes up to 100 sessions at a time.
A session page gets "Forget this session", and a fact page gets "Forget this key".
Each asks first, in a dialog with the sentence above and a link to the list below.
Studio hides only what the page showed you. Turns written after the page loaded stay visible.
Studio forgets sessions and fact keys. Use the shell for single records.

### Export

```sh
atticdb export ~/atticdb-export
```

It writes your memory as markdown. Facts go in `facts.md`, and each session gets `sessions/<agent>/<session>.md`.
It all comes from 1 point in time. Forgotten turns show as `## #<seq> forgotten`. Forgotten facts are left out.
The folder must not exist yet. A failed run never leaves a half export there.
The files hold your memory in plain text. A later forget doesn't touch them.
[`atticdb export`](crates/atticdb/README.md#atticdb-export) has the details.

### What a forget leaves visible

A forget hides turn content, meta, role and time, and fact values. These stay observable:
- Names: agents, sessions, fact keys and file-ref paths. `/v1/sessions`, `records` placeholders, studio's tables and list cursors show them.
- Positions: LSNs, seqs and record kinds. Placeholders keep them. Search and list cursors may name a hidden LSN or key.
- Counts: each session's `turns`, session totals, and the `/v1/stats` and `/metrics` fields `facts`, `fact_revisions` and `compressed_turns`. They include forgotten records. `search_index_terms` includes them until the next restart.
- Sizes and times: `atticdb dump` prints every record's payload length and commit time.
- Retry oracles: a retry with a guessed payload and a known retry key confirms the guess. `turn_repeats` counts repeated content.
- RAM: in-memory index tokens stay until the next restart. Hot cache entries stay until purged or evicted. Reads never serve either.
- Bytes: segment files, unsalted retry digests in checkpoints, filesystem and SSD remanence, copies, backups and exports already taken.
- `recover --truncate-at` below a forget's LSN drops the forget, and the content shows again.
- A retry that arrives after its key left the dedup window writes the content again.

## Uninstall

1. Run `atticdb disconnect <app>` for each app you connected: `copilot`, `vscode`, `claude-code`, `codex`, `gemini`, `antigravity` or `claude-desktop`. It removes only what setup added.
2. Stop the background server. Its pid is in the `LOCK` file in the data folder. The data folder is `~/Library/Application Support/atticdb` on macOS and `~/.local/share/atticdb` on Linux.

   ```sh
   cat ~/Library/Application\ Support/atticdb/data/LOCK   # macOS. Linux: ~/.local/share/atticdb/data/LOCK
   kill <pid>                                             # the number after pid=
   ```

3. Remove the binary and the data folder. The data folder holds all your memory, so this deletes it for good.

   ```sh
   rm ~/.local/bin/atticdb                         # or: cargo uninstall atticdb
   rm -rf ~/Library/Application\ Support/atticdb   # macOS
   rm -rf ~/.local/share/atticdb                   # Linux
   ```

If you set `ATTICDB_HOME`, that is the data folder. On Linux with `XDG_DATA_HOME` set, it is `$XDG_DATA_HOME/atticdb`. If you set `ATTICDB_INSTALL_DIR`, the binary is there.

## Where to run it

macOS, Linux and Windows x86_64. Release binaries cover macOS and Linux on x86_64 and arm64, and Windows on x86_64. The Linux ones are static, so any distro works. Then `atticdb setup` connects it to VS Code, Copilot CLI, Claude Code, Codex, Gemini CLI, Antigravity and Claude Desktop.
Claude Desktop runs only on macOS and Windows, so Linux can't connect it.
On Windows, Codex gets only its MCP entry for now ([#142](https://github.com/Clawdlinux/atticdb/issues/142)). On Windows, Claude Code 2.1.139 or later is needed for the hook. Durability there is weaker, as Windows has no directory fsync. [Windows](crates/atticdb/README.md#windows) lists what differs.

Keep the data directory on a local disk. Every ack waits for a sync, so that disk sets your write latency.
[Where to keep the data directory](crates/atticdb/README.md#where-to-keep-the-data-directory) lists the disks we tested and the ones to avoid.

## Tell us what you need

AtticDB does a few things on purpose. We add features when people ask for them.

- Missing an app, a command or an API? [Open a feature request](https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml). Say what you're trying to do, not just the feature.
- Something broke? [Open a bug](https://github.com/Clawdlinux/atticdb/issues/new?template=bug.yml) with the `atticdb doctor` output.
- Someone asked already? Add a thumbs-up to that issue. We count those. Issues labeled `waiting-for-users` are ideas we parked until enough people want them.

## Docs

- [atticdb](crates/atticdb/README.md): server, HTTP API, MCP, studio, `tree`, `forget`, `export`, `stats`, `load`, `verify`, `dump` and `recover`. It also has these:
  - [Run a server by hand](crates/atticdb/README.md#run-a-server-by-hand), [Keys](crates/atticdb/README.md#keys) and [Where to keep the data directory](crates/atticdb/README.md#where-to-keep-the-data-directory).
  - [MCP](crates/atticdb/README.md#atticdb-mcp), [Secrets](crates/atticdb/README.md#secrets), [Shell verbs](crates/atticdb/README.md#shell-verbs) and [Connect](crates/atticdb/README.md#atticdb-connect-and-atticdb-disconnect).
  - [Studio](crates/atticdb/README.md#atticdb-studio), [`atticdb export`](crates/atticdb/README.md#atticdb-export) and [Windows](crates/atticdb/README.md#windows).
- [atticdb-core](crates/atticdb-core/README.md) and [atticdb-tape](crates/atticdb-tape/README.md): engine, formats, recovery and limits.
- [Overview](docs/overview.md): how it works, benchmark numbers and the build steps.
- [Sandboxed agents](docs/deploy/sandboxes.md): who holds memory, and how sandboxed code can reach it.
- [Python client](clients/python/README.md), [TESTING.md](TESTING.md) and the [decision log](docs/design/0002-decision-log.md).
- Benchmarks: [head-to-head tables](bench/H2H.md) and [macOS engine runs](bench/RESULTS.md). History: [0001 (historical)](docs/design/0001-architecture.md).

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before you open a pull request. It has the build and test commands.
Follow the [code of conduct](CODE_OF_CONDUCT.md), and report vulnerabilities as [SECURITY.md](SECURITY.md) says.

## License

License: MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
