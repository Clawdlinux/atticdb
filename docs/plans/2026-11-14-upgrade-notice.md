# Upgrade Notice at Chat Start and Opt-in Auto-upgrade

**Goal:** You learn about a new release without running `atticdb doctor`. You can let AtticDB install releases on its own.

**Spec:** GitHub issue Clawdlinux/atticdb#230. Amends D105 as D115.

## Why

The notice prints only from `doctor` and `setup`. People rarely run either. The owner asked for a visible notice with the command, and an option to upgrade on its own.

Also, `atticdb upgrade` refused a release binary in `~/.cargo/bin`. The owner's install is one. So no upgrade path worked there.

## Design

1. **Notice in the chat-start block.** The hook reads `<home>/update-check.json` only. It never asks the network. Off switches hide it. `+dev` builds never show it. The line sits right after the header, so the cap never drops it.
2. **The home server refreshes the cache.** `serve` starts 1 thread when its data dir is `<home>/data`. Every hour it calls `update::latest`, which asks at most once a day and backs off 1 h after a failure. Other servers, hosted or sandboxed, never check.
3. **Opt-in auto-upgrade, Unix only.** `atticdb upgrade --auto on` writes `<home>/auto-upgrade`. `--auto off` removes it. With it on, the thread above runs `<this binary> upgrade --yes --version <v>` detached, in its own process group, when a newer release is out. Output appends to `<home>/upgrade.log`. A version is tried at most once a day per server. `+dev` builds never run it. `upgrade` already stops the verified server and runs the new doctor, which starts the new server.
4. **`managed()` fix.** cargo builds print `+dev`. So a release version in `~/.cargo/bin` came from `install.sh`, and `upgrade` handles it.

## Not doing

- No silent default. Auto stays off until you turn it on.
- No upgrade from the hook or `mcp`. They stay offline.
- No Windows auto-upgrade. A running `atticdb.exe` can't be replaced.
- No store format version check. Releases carry none. `atticdb.prev` stays the rollback.

## Tests

- Unit: the block notice line, auto on and off, `+dev`. `managed()` with release and `+dev` versions.
- Integration: the hook never asks the network, even with a stale cache. The home server refreshes the cache. A `+dev` server with auto on never fetches `install.sh`. `upgrade --auto on|off` writes and removes the marker. Bad values are usage errors.
