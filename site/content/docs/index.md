AtticDB is an agent memory database in Rust. It stores turn history per session and keyed facts. A write is acked only after it is synced to disk.

Status: early. Don't run it in production. Built so far: the tape, session engine, disk-full handling, server and facts (S1 to S4).

## Where to run it

macOS, Linux and Windows x86_64. Release binaries cover macOS and Linux on x86_64 and arm64, and Windows on x86_64. The Linux ones are static, so any distro works. Then `atticdb setup` connects it to VS Code, Copilot CLI, Claude Code and Codex.

On Windows, Codex gets only its MCP entry for now ([#142](https://github.com/Clawdlinux/atticdb/issues/142)). On Windows, Claude Code 2.1.139 or later is needed for the hook. Durability there is weaker, as Windows has no directory fsync. [Windows](docs:windows) lists what differs.

## Pick a disk

Every write is acked only after `fdatasync`. So the disk under the data directory sets your write latency. Keep it on a local disk:

| Data directory on | Status |
|---|---|
| Local NVMe or SATA SSD | Tested on NVMe with Linux ext4. An ack costs about 1 device round trip, about 60 µs on NVMe. macOS APFS is tested for development |
| Network block SSD, such as Azure Premium SSD v2 or P30 | Tested with Linux ext4. An ack costs 1 device round trip: about 1.9 ms on Premium SSD v2 and 4.4 ms on P30 |
| HDD | Untested. It should work, but each ack waits for a sync to a spinning disk, which takes milliseconds |
| NFS, SMB, FUSE mounts, or folders synced by iCloud Drive, Dropbox, OneDrive or Google Drive | Don't. `fsync` and `flock` guarantees vary on these, and sync clients copy files while they're being written |

The data directory must be mode `0700` (D75). Windows has no modes, so there it must be inside the user profile (D92). Below a free-space floor AtticDB goes read-only and keeps serving reads (I14).

## In these docs

- [Install](docs:install): the installer, `atticdb setup` and `atticdb doctor`.
- [Connect your apps](docs:connect): VS Code, Copilot CLI, Claude Code and Codex.
- [MCP tools](docs:mcp): what `atticdb mcp` gives the model.
- [Shell verbs and studio](docs:cli): the same tools from a shell, and the read-only page.
- [Server and HTTP API](docs:http-api): `atticdb serve`, its routes and the outcome rule.
- [How it works](docs:how-it-works): the tape, group commit and recovery.
- [Windows](docs:windows): what differs there.

Source: [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md)
