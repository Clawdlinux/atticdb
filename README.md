# AtticDB

Agent memory database in Rust. It stores turn history per session and keyed facts. A write is acked only after it is synced to disk.
Status: early. Don't run it in production. Built so far: the tape, session engine, disk-full handling, server and facts (S1 to S4).

## Where to run it

macOS, Linux and Windows x86_64. Release binaries cover macOS and Linux on x86_64 and arm64, and Windows on x86_64. The Linux ones are static, so any distro works. Then `atticdb setup` connects it to VS Code, Copilot CLI, Claude Code and Codex.
On Windows, Codex gets only its MCP entry for now. Durability there is weaker, as Windows has no directory fsync. [Windows](crates/atticdb/README.md#windows) lists what differs.

Every write is acked only after `fdatasync`, so the disk under the data directory sets your write latency. Keep it on a local disk:

| Data directory on | Status |
|---|---|
| Local NVMe or SATA SSD | Tested on NVMe with Linux ext4. An ack costs about 1 device round trip, about 60 µs on NVMe. macOS APFS is tested for development |
| Network block SSD, such as Azure Premium SSD v2 or P30 | Tested with Linux ext4. An ack costs 1 device round trip: about 1.9 ms on Premium SSD v2 and 4.4 ms on P30 |
| HDD | Untested. It should work, but each ack waits for a sync to a spinning disk, which takes milliseconds |
| NFS, SMB, FUSE mounts, or folders synced by iCloud Drive, Dropbox, OneDrive or Google Drive | Don't. `fsync` and `flock` guarantees vary on these, and sync clients copy files while they're being written |

The data directory must be mode `0700` (D75). Windows has no modes, so there it must be inside the user profile (D92). Below a free-space floor AtticDB goes read-only and keeps serving reads (I14).

## Quick start

```sh
curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"   # needed only if the installer said it isn't on PATH
atticdb setup --dry-run   # shows what it would change, writes nothing
atticdb setup             # asks which apps, then wires them
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

`setup` finds Copilot CLI, VS Code, Claude Code and Codex. It adds the MCP entry, a chat-start hook and the guide each app reads. Every edited file is backed up first. `atticdb disconnect <app>` removes exactly what it added. Codex runs a new hook only after you approve it once in Codex. [research/setup_eval/results](research/setup_eval/results/README.md) has the eval results.

`connect` stores the installed binary's path in the entry. An entry that points into a checkout's `target/` breaks on `cargo clean`.

To run a server by hand:

```sh
cargo build -p atticdb --release
export PATH="$PWD/target/release:$PATH"
TOKEN=$(atticdb init --config atticdb.json --tenant me)   # 0600 config, token printed once
atticdb serve --config atticdb.json --data-dir ./data &    # TCP 127.0.0.1:7425 and ./data/atticdb.sock
curl -s -H "Authorization: Bearer $TOKEN" \
  -d '{"agent":"planner","session":"run-1","idem_key":"k1","turns":[{"role":"user","content":"hi"}]}' \
  http://127.0.0.1:7425/v1/append
curl -s --unix-socket data/atticdb.sock -H "Authorization: Bearer $TOKEN" \
  'http://localhost/v1/window?agent=planner&session=run-1&last=10'
```

That appends over TCP and reads over the Unix socket. A repeated `idem_key` returns `"deduplicated": true`. Facts live at `/v1/facts/put`, `get`, `list` and `delete`. Each key holds 1 current value, and a later put replaces it. With `"if_lsn": <lsn>`, a put writes only if the current value still has that LSN. A mismatch is 409 `conflict`.

## Tell us what you need

AtticDB does a few things on purpose. We add features when people ask for them.

- Missing an app, a command or an API? [Open a feature request](https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml). Say what you're trying to do, not just the feature.
- Something broke? [Open a bug](https://github.com/Clawdlinux/atticdb/issues/new?template=bug.yml) with the `atticdb doctor` output.
- Someone asked already? Add a thumbs-up to that issue. We count those. Issues labeled `waiting-for-users` are ideas we parked until enough people want them.

## MCP

`atticdb mcp` gives the model `memory_append`, `memory_recent`, `fact_set`, `fact_get`, `fact_list` and `memory_search`. Search finds turns and current facts by words, across every session, newest first. It starts the server if none is running.
The model can read and write any session in the tenant. Don't give it to a model you don't trust with the whole tenant.

The server's instructions tell the model when to read memory and what to save. Claude Code and Codex read them.

### Secrets

Before `memory_append` and `fact_set` write, AtticDB replaces secrets in the content and in every `meta` string with placeholders such as `[secret:github-token]`.
The shell verbs `append` and `fact set` do the same. The result's `redacted` field counts what was replaced, by type.
It looks for these:

- the values of env vars whose names end in `_KEY`, `_TOKEN`, `_SECRET` or `_PAT`, or contain `PASSWORD`, if 12 bytes or longer;
- GitHub tokens, AWS key ids, Slack tokens, Stripe live keys, Anthropic and OpenAI keys;
- private key blocks, `Bearer` tokens, and passwords in URLs.

It is best effort. A secret that was changed, split or encoded gets through, so don't save secrets.

```json
{"mcpServers": {"atticdb": {"command": "atticdb", "args": ["mcp"]}}}
```

With no connection flags, `atticdb mcp` and the shell verbs use a per-user home, created on first use.
On macOS it is `~/Library/Application Support/atticdb`. On Linux it is `$XDG_DATA_HOME/atticdb`, else `~/.local/share/atticdb`.
`ATTICDB_HOME` moves it, for example to a test folder. It must be an absolute path. Empty counts as unset.
It holds `atticdb.json` with tenant `me`, and `data/`. Any connection flag skips the home. Use absolute paths in flags.
With no connection flags, `ATTICDB_TOKEN` is ignored and a note says so. The home's token is used.

## Connect

Add AtticDB to VS Code's Copilot chat or to Copilot CLI with 1 command:

```sh
atticdb connect vscode       # or: atticdb connect copilot
atticdb disconnect vscode    # removes only AtticDB's entry
```

It edits the user MCP config and keeps a backup of the old file. The entry holds no token.
Under `ATTICDB_HOME`, the entry and the hook pass it on, so the harness uses that home too.
VS Code and Copilot CLI share the hook, so `connect` refuses to give them different homes.
Notes go to a session named after the repo, so each repo keeps its own.
VS Code reports its workspace folders, and the first one is the repo.
Copilot's agent host and Copilot CLI start AtticDB in the working folder, which is the repo.
Turns saved before this with no `--session` stay in session `default`. Read them with `--session default` and the agent that saved them.
That is `--agent copilot` for the `connect copilot` entry. A bare `atticdb mcp` saved them as agent `mcp`, so pass no `--agent`. Or find them with `search`.
After upgrading AtticDB, run `connect` again. It reports `updated` when the entry changed.
An old entry with connection flags, such as `--data-dir`, keeps its own store. `connect` refuses to switch it without `--force`.
To keep that memory, quit VS Code and Copilot CLI, then stop that store's server.
Move its data files into the home's empty `data/`. This works only if its tenant is `me`.
Then, or to switch without that memory, run `atticdb connect <name> --force`.

It also adds a Copilot hook file, `~/.copilot/hooks/atticdb.json`. VS Code's Copilot chat and Copilot CLI both read it.
At the start of each chat, the hook adds a small memory block.
It lists your `user/` rules and this repo's facts.
It adds the 5 newest notes in this repo's session from agents `vscode-copilot`, `copilot` and `mcp`.
`mcp` is the agent of the shell verbs, so a note saved with `atticdb append` shows too.
The block labels notes as history, not instructions.
The repo is the folder holding `.git`, at or above the chat's working directory.
A worktree counts as its main repo, so it shares that repo's memory.
The block is at most 6,000 characters. Each content is cut to 300 characters.
It is a snapshot. The model can call `memory_search` or `fact_list` for more.
To turn it off, delete the hook file, or disconnect both harnesses.

`connect` also writes the server's instructions to `~/.copilot/instructions/atticdb.instructions.md`, as Copilot may not read them from the server.
The file holds the line `<!-- written by atticdb connect -->`. Without it, the file is yours, and AtticDB never changes or removes it.
`connect` also skips the file when another `.instructions.md` file there mentions AtticDB, and says which.
Disconnecting both harnesses removes it, unless you edited it.

## Shell verbs

The same 6 tools run from a shell, with `atticdb mcp`'s connection flags. Each prints 1 JSON line. `fact set --delete` deletes.

```sh
export ATTICDB_TOKEN=$TOKEN
Z="--url unix://$PWD/data/atticdb.sock --agent planner --session run-1"
atticdb append $Z --content "hi" --role user          # {"committed":true,"deduplicated":false,"lsn":3,"seq":2}
atticdb recent $Z --n 10                              # {"truncated":false,"turns":[...]}
atticdb fact set $Z --key repo/payments/retry-rule --content "Retry 3 times, then fail."
atticdb fact get $Z --key repo/payments/retry-rule    # {"fact":{"agent":"planner","content":...}}
atticdb fact list $Z --prefix repo/ --limit 50        # {"facts":[...],"next":null}
atticdb search $Z --query "retry rule"                # {"hits":[...],"next":null}
```

Each write picks 1 random `idem_key` and retries with it until `--retry-deadline` (default 30 s). Exit 0 means done. Exit 1 means nothing was written, or a read failed. Exit 2 is a usage error.
Exit 3 means the write may have landed. Running the command again uses a new key, so it can write a duplicate.

## Studio

`atticdb studio` serves a read-only web page about your memory, on loopback, and opens it in your browser.

```sh
atticdb studio             # the home, opens your browser
atticdb studio --no-open   # only prints the link
```

It takes `atticdb mcp`'s connection flags. With none, it uses the home and starts its server if needed.
Each run listens on `127.0.0.1` on a new random port, and prints a link with a secret. Stop it with Ctrl-C.
Snap browsers, the default on Ubuntu, cannot open the page from the hidden home folder, so they show an error.
Then open the printed link by hand, or pass `--no-open`.
The page needs that secret to read anything. The tenant token never leaves the studio process.
It never writes to the store. Details: [atticdb studio](crates/atticdb/README.md#atticdb-studio).
The page has 7 screens: Overview, Log, Sessions, Facts, Search, Chat start and Health.
To try it on realistic memory, run `crates/atticdb/studio/seed.sh <empty dir>`, then `HOME=<dir> XDG_DATA_HOME= ATTICDB_HOME= atticdb studio`.

## Numbers

Durable appends, `fsync` before every ack, Azure L8s v3. Medians of 7 interleaved reps, 1 client for every server. Full tables: [bench/H2H.md](bench/H2H.md).

| Local NVMe, 16 writers, closed loop | atticdb | redis | valkey | dicedb |
|---|---|---|---|---|
| Acks/s | 48,611 | 44,572 | 42,928 | 44,693 |
| p99 µs | 502 | 549 | 573 | 547 |

Of the 4 servers, AtticDB has the best closed-loop acks/s and p99 on both disks. On Premium SSD v2 it beats Redis, Valkey and DiceDB in every cell. At 16 writers it acks 9,328/s against 5,086 to 5,151. On local NVMe it's mixed:
- Closed loop, 1 and 4 writers: AtticDB beats Redis, Valkey and DiceDB on every median. It acks 9,799 and 24,516/s, against at most 8,436 and 18,444.
- Closed loop, 16 writers: Redis and DiceDB have a lower p50, 299 and 300 µs against 305. AtticDB's p99.9 and max are the worst of the 4: 674 and 1,210 µs. The others get 604 to 624 and 810 to 947.
- Agent-shaped, 64 writers at 5 ms think and 256 at 50 ms: best p50, 148 and 138 µs. The others get 182 to 184 and 140 to 145. Its p99.9 is best too: 786 and 736 against 964 to 1,053 and 2,103 to 2,758.
- AtticDB's agent-shaped p99 median trails. At 64 writers it's 416 µs against Redis's 382. At 256 it's 558 against DiceDB's 348 and Valkey's 496. [#47](https://github.com/Clawdlinux/atticdb/issues/47) tracks it.
- Those medians sit inside run-to-run noise. At 256 writers AtticDB's p99 ranged 225 to 636 µs and DiceDB's 289 to 1,078. Per rep in the TSVs, AtticDB's 256-writer p99 beat Redis in 7 of 7. It beat Valkey in 4 and DiceDB in 5.
- At 256 writers AtticDB's max is the worst, 8,698 µs against 7,392 to 8,039. Acks/s ties, since think time sets the rate. AtticDB's median is lowest, by 0.5 to 1.6 acks/s.

In-process SQLite wins some cells. At 1 writer on NVMe it acks 19,410/s against AtticDB's 9,799. Its p50 is lower at every NVMe load: 32 µs at 1 writer, 57 and 68 under agent load. On NVMe its p99 is lower at 4 and 16 writers, and its p99.9 at 16. On Premium SSD v2 its closed-loop p50 is lower, and its p99 and p99.9 at 4 and 16.

## How it works

- 1 append-only tape is both WAL and store. Indexes and checkpoints are derived and rebuild from it (D1, I8).
- A zero-filled spare segment is kept ready for the next roll. On ext4 an `fdatasync` is then usually 1 device round trip (D78).
- Group commit never waits on a timer. The writer syncs whatever is queued when it wakes (D80).
- Frames carry location-seeded CRC32C and footers bind whole transactions. Recovery cuts only an unproven tail (D64, D65, D8). Damage inside the last synced group has no later proof, so it can still be cut (I1).
- Windows cost O(returned). Paged arrays, held to a memory budget, map seqs to LSNs and LSNs to offsets (D2, D66, I5).
- Every write carries an `idem_key`, so a retry in the dedup window can't commit twice (D67, I6). A client that can't tell whether a write landed reports `unknown`, never `not_committed` (D75).
- Below a free-space floor the engine goes read-only. Writes are rejected before anything is written. Reads keep working (I14).

D-ids are in the [decision log](docs/design/0002-decision-log.md). I-ids are invariants in [0003](docs/design/0003-slim-v0.1.md), which defines the current scope.
CI runs both SimDisk crash simulations on 2,000 seeds. Anyone with write access to the data directory can edit history. Nothing detects it.

## Sandboxed agents

The harness is the trusted loop that calls the model and starts sandboxes. By default it holds memory (D79). It reads the window and facts, builds the prompt and appends results.
Code in a microVM or container gets no token and no socket, because a token grants a whole tenant. Direct access ([sandboxes.md](docs/deploy/sandboxes.md)) is for code you trust with that tenant, or code that has its own tenant.

## Keys

Each tenant has 1 key space. Keys are 1 to 1024 bytes and never go in URL paths. Agents keep keys apart with prefixes such as `repo/<area>/`. A prefix matches bytes, so `repo/pay` also matches `repo/payments/`. End it with `/` to match whole path segments.

## Steps

| Step | Scope |
|---|---|
| S1 | Frame v2, guarded recovery, bounded tape index. |
| S2 | Disk-full floor and real preallocation. |
| S3 | Shared server, auth, HTTP, MCP and Python client. |
| S4 | Tenant-scoped fact revisions and conditional updates. |
| S5 | Hide requests and markdown export. |
| S6 | Dogfood and measure the remaining gaps. |

## Build and test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace && cargo test --workspace --release
ATTICDB_SIM_SEEDS=2000 cargo test -p atticdb-tape --release --features sim --test sim
ATTICDB_SIM_SEEDS=2000 cargo test -p atticdb-core --release --features sim --test engine_sim
```

## Docs

- [atticdb](crates/atticdb/README.md): server, HTTP API, MCP, studio, `stats`, `load`, `verify`, `dump` and `recover`.
- [atticdb-core](crates/atticdb-core/README.md) and [atticdb-tape](crates/atticdb-tape/README.md): engine, formats, recovery and limits. [Python client](clients/python/README.md), [TESTING.md](TESTING.md), [macOS engine runs](bench/RESULTS.md), [0001 (historical)](docs/design/0001-architecture.md).

License: MIT OR Apache-2.0.
