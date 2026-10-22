# Offline verify and dump. Implementation plan

> For agents: one implementer on branch `feat/cli-verify`, TDD, one commit per task. Then a spec review and a quality review before the PR.

**Goal:** debug a stopped data directory without changing it. Covers issues #11 (verify) and #12 (dump). #10 (inspect) folds into verify, because its output is a subset of what verify prints.

**Base:** `main` at `fdf4225`.

**Rules:** no new crates, no `unsafe`, no bypassed checks. Tests follow `TESTING.md`. Short sentences, no em dashes, one-line comments. Commits use `git commit -s` with the Copilot trailer. Do not push.

**Commands:** the seven Rust commands from the S4 plan, with sim seeds at 2000, plus the Python client tests.

## Design

### D1. A dry-run `Io`

Opening a data directory repairs it. Recovery zero-fills torn tails and rewrites bytes (`recovery.rs:647-680`). `DataDir::open` removes orphan segments and spares (`dir.rs:270-280`) and rewrites the `LOCK` holder line (`dir.rs:311-314`). The engine writes checkpoints and the tape fills a spare. A second, read-only recovery would drift from the real one, so we don't write one.

Instead, `DryRunIo` in `crates/zerodb/src/dryrun.rs` wraps `StdIo`, following the `FaultIo` precedent (`fault.rs`):

- **Reads** go to disk, unless an earlier write to that path was buffered.
- **Writes, `set_len`, `allocate`, `create`, `rename`, `remove`** change an in-memory copy. The first change to a real file copies it into memory first. Each changed path is recorded once.
- **`sync_data` and `sync_dir`** do nothing.
- **`try_lock_exclusive`** goes to disk. Holding the real lock stops a server from starting mid-read, and a running server makes the tool exit 75, like `recover`.
- **`available_bytes`** returns 0, so open never fills a spare in memory.

I am choosing a buffering `Io` over a read-only scanner because verify then reports exactly what an open would do, with one recovery implementation.

### D2. `zerodb verify --data-dir <dir>`

- Opens `Engine::open_with(DryRunIo, SystemClock, dir, EngineOptions::default())`, the options `serve` uses.
- On success it prints sorted `key=value` lines, built by the same code as `GET /v1/stats` (factor the JSON out of `api::stats`). Then it prints `changes=<n>` and one `change=<path>` line per path the open would have written, apart from the `LOCK` holder line, which every open rewrites.
- Exit codes: 0 opened, 1 the open failed (corruption, invalid configuration), 75 locked.

### D3. `zerodb dump --data-dir <dir> [--from <lsn>] [--limit <n>]`

- Opens `Tape::open(DryRunIo, SystemClock, dir, TapeOptions::default())` and walks `scan(from)`.
- One line per record: `lsn txn kind tenant bytes commit_time_ns`. Never content.
- Exit codes match verify.

### D4. Memory

The in-memory copy holds only what an open would change. That is at most the repaired segments (64 MiB each) plus the LSN index and checkpoint files. The README says so.

## Tasks

| Task | Change | Tests |
|---|---|---|
| T1 | `DryRunIo` | Unit tests in `dryrun.rs` (private logic, per TESTING.md 4): one table covering a read-after-write overlay, `remove` then `exists`/`list`, `rename`, and `set_len` growth and shrink, against a real temp dir whose bytes must not change |
| T2 | `zerodb verify` and the shared stats JSON | Extend the `startup_checks` Recovers row (`tests/it/listen.rs`). After the kill, `verify` exits 0 with `durable_lsn` equal to the last acked LSN, and every file under the data dir is byte-identical before and after. Under the running server it exits 75 |
| T3 | `zerodb dump` | Same row: `dump --from <first lsn>` prints the acked records in LSN order, and `--limit 1` prints one |
| T4 | A corrupted directory makes `verify` exit 1 | Extend the same row: flip one byte inside a synced frame below the superblock on a copy of the data dir. `verify` exits 1 and names the LSN |
| T5 | Docs: `crates/zerodb/README.md`, top README list, close #10 as covered by verify | |

## Not in this plan

Repair. `zerodb recover --truncate-at` already does that, and verify's error names the LSN to cut at.
