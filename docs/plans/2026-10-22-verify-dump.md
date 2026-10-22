# Offline verify and dump. Implementation plan

> For agents: one implementer on branch `feat/cli-verify`, TDD, one commit per task. Then a spec review and a quality review before the PR.

**Goal:** debug a stopped data directory without changing it. Covers issues #11 (verify) and #12 (dump). #10 (inspect) folds into verify: verify prints the post-recovery metadata inspect would. A directory verify cannot open reports the error and LSN, which is what you act on.

**Base:** `main` at `fdf4225`.

**Rules:** no new crates, no `unsafe`, no bypassed checks. Tests follow `TESTING.md`. Short sentences, no em dashes, one-line comments. Commits use `git commit -s` with the Copilot trailer. Do not push.

**Commands:** the seven Rust commands from the S4 plan, with sim seeds at 2000, plus the Python client tests.

## Design

### D1. Verify a copy, never the original

Opening a data directory repairs it. Recovery zero-fills torn tails and rewrites bytes (`recovery.rs:647-680`). `DataDir::open` removes orphan segments and spares (`dir.rs:270-280`) and rewrites the `LOCK` holder line (`dir.rs:311-314`). The engine and tape start workers that save checkpoints and fill a spare. A second, read-only recovery would drift from the real one.

So both commands:

1. Open the original `LOCK` without `create` and take the `flock` with `File::try_lock`, as `serve::lock_holder` does (`serve.rs:385-391`). Held means exit 75. Missing means "not a data directory", exit 1. This stops a server from starting mid-copy, and nothing on the original is created or written.
2. Copy every regular file (the data directory is flat) into a new scratch directory with `std::fs::copy`. That clones on APFS, and on Linux `copy_file_range` reflinks on btrfs and XFS. Skip the socket and other non-regular files.
3. Run the real open on the copy. Report. Remove the copy, even on error.

The scratch directory defaults to `<data_dir>.verify-<8 hex>`, mode 0700, on the same filesystem so clones work. `--scratch <dir>` puts it elsewhere, for a disk too full to hold a copy.

I am choosing a copy over an in-memory dry-run `Io` because review found that would need an offline-open mode with no workers, a sparse overlay with a memory limit, and lock changes. The copy runs the exact production open, threads included. The cost is one directory's worth of time and space, or none where the filesystem clones.

### D2. `zerodb verify --data-dir <dir> [--scratch <dir>]`

- Opens `Engine::open(copy, EngineOptions::default())`, the options `serve` uses.
- Prints sorted `key=value` lines from the same code as `GET /v1/stats`. Factor the JSON out of `api::stats` into a function both use.
- Exit codes: 0 opened, 1 the open failed or a checkpoint was rejected, 75 locked. A failed open names the first bad LSN; `recover --truncate-at` takes a transaction boundary below it.

### D3. `zerodb dump --data-dir <dir> [--scratch <dir>] [--from <lsn>] [--limit <n>]`

- Opens `Tape::open(StdIo, SystemClock, copy, TapeOptions::default())` and walks `scan(from)`.
- One line per record: `lsn txn kind tenant bytes commit_time_ns`. Never content.
- Exit codes match verify.

## Tasks

| Task | Change | Tests |
|---|---|---|
| T1 | Lock, copy and cleanup helper in `ops.rs`. `zerodb verify` and the shared stats JSON | Extend the `startup_checks` Recovers row (`tests/it/listen.rs`). Under the running server, `verify` exits 75. After the kill, `verify` exits 0 with `durable_lsn` equal to the last acked LSN. Every file under the data dir is byte-identical before and after, and no scratch directory is left behind |
| T2 | `zerodb dump` | Same row: `dump --from <first acked lsn>` prints the acked records in LSN order, and `--limit 1` prints one |
| T3 | Corruption | End of the same row, after the restarted server stops. Its open installed the superblock at its durable LSN (`writer.rs:399-401`), so LSN 1 is below it. Flip one payload byte of the first frame in `seg-0000000000000001.zdb`. `verify` exits 1 and names LSN 1 |
| T4 | Docs: `crates/zerodb/README.md` and the top README list. #10 is covered by verify | |

## Not in this plan

Repair. `zerodb recover --truncate-at` already does that. verify names the first bad LSN, and recover names the nearest boundaries for an interior one.
