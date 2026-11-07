# Smaller segments for new homes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new home store reserves about 34 MB instead of 130 MB. Existing stores open exactly as before, and doctor shows what the store holds on disk.

**Architecture:**
- **Tape.** The segment size a store was created with, stored in its MANIFEST, is authoritative. The configured size only applies to a new directory. Today a mismatch is an error.
- **Server.** A new config key, `segment_mib`, sets the size for new stores. Homes made by `atticdb setup` or the first run get `"segment_mib": 16` in their config. `atticdb init`, used for servers, keeps the 64 MiB default.
- **Doctor.** It prints 1 line with the store's size on disk and its segment size.

**Tech Stack:** Rust: `atticdb-tape`, `atticdb-core` and `atticdb`. Engine sims and the fault-inject suite run in CI.

**Spec:** Issue #192 and its findings comment from 2026-11-06.

## Global Constraints

- **16 MiB floor.** Requests are capped at 8 MiB (`http.rs` `Limits::body_bytes = 8 << 20`). A batch must fit within 1 segment (`writer.rs:911`, `BatchTooLarge`). 16 MiB is 2x the body cap.
  - `segment_mib` must be at least 16 and at most 1024. Anything else is a config error naming the key and the range.
- **Existing stores.** A store's stored segment size always wins over the configured size. Opening a store never fails because the sizes differ, and nothing rewrites the stored size.
- **Defaults.**
  - `TapeOptions::default().segment_size` stays 64 MiB.
  - A config without `segment_mib` behaves exactly as today.
  - Only `home.rs` `publish` writes `segment_mib`, with the value 16.
- **Doctor line.** Under "Server and store": `store: <X> MB on disk, <S> MiB segments`.
  - `<X>` is the sum of regular-file sizes in the data dir, in MB (10^6 bytes) with 1 decimal place.
  - `<S>` comes from the MANIFEST, read without opening the engine. It's informational, never a failure.
- **Writing and commits.** No em dashes, sentences under 20 words. Commits use `git commit -s`, with no Co-authored-by.
- **Verify.**
  - `cargo fmt --all --check`
  - `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
  - `cargo clippy --locked --workspace -- -D warnings`
  - `cargo test --locked --workspace`
  - `cargo test --locked --workspace --release`
  - `ATTICDB_SIM_SEEDS=2000 cargo test --locked -p atticdb-tape --release --features sim --test sim`
  - `ATTICDB_SIM_SEEDS=2000 cargo test --locked -p atticdb-core --release --features sim --test engine_sim`
  - `cargo test --locked -p atticdb --release --features fault-inject --test it -- --include-ignored`
  - Python: `cargo build --locked -p atticdb --release --features fault-inject && ATTICDB_BIN=target/release/atticdb PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 python3 -m pytest clients/python/tests research/setup_eval/tests research/handoff_eval/tests scripts/tests -q`

## Review Focus

1. **An existing 64 MiB home opens unchanged.** The store was written by today's binary and opened with a config that says 16. It keeps 64 MiB segments and its spare, and every record stays readable. Test: `existing_store_keeps_its_segment_size` (Task 1, tape level, and Task 2, server level).
2. **A batch near the 8 MiB body cap is accepted by a 16 MiB home.** Test: `home_accepts_a_body_cap_batch` (Task 2).
3. **Crash recovery and the spare follow the stored size, never the configured one.** Test: a sim or tape test that reopens after a crash with a different configured size (Task 1).
4. **A bad `segment_mib`** (0, 15, 1025, a string, negative) is a clear config error. Test: `segment_mib_range` (Task 2).
5. **Doctor's line works on a store whose server isn't running.** Test: `doctor_shows_store_size` (Task 3).

---

### Task 1: tape opens an existing store with its stored segment size

**Files:**
- Modify: `crates/atticdb-tape/src/dir.rs` (`open_inner` near line 280-330, `check_segment_size` at 393), `crates/atticdb-tape/src/writer.rs` (`Tape::open` around 549-596, which passes `opts.segment_size` to `recover_with_budget` and `SegmentWriter::new`), `crates/atticdb-tape/src/recovery.rs:467`, `crates/atticdb-tape/src/segment.rs:91`
- Test: `crates/atticdb-tape` unit tests and `tests/it`. Update any test that expects the "differs from stored" error.

**Interfaces:**
- `DataDir::open_versioned(io, path, address, segment_size, manifest_new)` keeps its signature. `segment_size` now means "the size for a new directory".
- For an existing MANIFEST, `dir.segment_size()` returns the stored value, and the writer uses `dir.segment_size()` from then on. `check_segment_size` either goes away or is used only where a caller really needs to assert sizes. Decide by reading its 4 call sites, and say what you chose.
- The size check in `Tape::open` (`segment cannot hold a transaction`) still applies to the configured size, for new directories.

- [ ] Write failing tests:
  - Create a store with a 64 KiB segment size and write records. Reopen it with a configured 32 KiB. It opens, `segment_size()` is 64 KiB, all records read back, and new appends fill 64 KiB segments.
  - Do the same after a simulated crash, using the existing crash or SimDisk helpers, so recovery and the spare also use 64 KiB.
  - A new directory uses the configured size.
- [ ] Run the tests and watch them fail.
- [ ] Implement.
- [ ] Run the tape and core suites and both sims at `ATTICDB_SIM_SEEDS=2000`.
- [ ] Commit: `fix(tape): an existing store keeps its stored segment size`.

### Task 2: `segment_mib` config, and 16 MiB for new homes

**Files:**
- Modify: `crates/atticdb/src/config.rs` (new key, plus the key list at line 25), `crates/atticdb/src/serve.rs` (around line 423, set `options.tape.segment_size`), `crates/atticdb/src/home.rs` `publish` (write `segment_mib: 16`, possibly through a new `config::init` parameter or a home-specific writer, whichever is less code)
- Test: `crates/atticdb/src/config.rs` unit tests, and `crates/atticdb/tests/it` (api or cli).

**Interfaces:**
- `Config::segment_mib: Option<u64>`.
- `config::init` writes for `atticdb init` exactly what it writes today.

- [ ] Write failing tests:
  - `segment_mib_range` covers 16 ok, 1024 ok, 15, 1025, 0, `"16"` and -1, each failing with a message naming the key.
  - A new home made by `home::ensure` has `"segment_mib": 16` in `atticdb.json`, and its first segment file and spare are 16 MiB.
  - `existing_store_keeps_its_segment_size`: a home created with a config without `segment_mib` holds 64 MiB segments. Add `segment_mib: 16` to its config and restart. It still serves its data and keeps 64 MiB files.
  - `home_accepts_a_body_cap_batch`: in a 16 MiB home, an append whose JSON body is just under 8 MiB commits.
  - `atticdb init` output is unchanged.
- [ ] Run the tests and watch them fail.
- [ ] Implement.
- [ ] Run the verify list.
- [ ] Commit: `feat: new homes use 16 MiB segments`.

### Task 3: doctor shows the store's size, and docs

**Files:**
- Modify: `crates/atticdb/src/doctor.rs`, `README.md`, `crates/atticdb/README.md` (the config key, the new home size, and the doctor line), and `docs/design/0002-decision-log.md` (D106)
- Test: the doctor tests in `crates/atticdb/tests/it/setup.rs`.

- [ ] Write a failing test: `doctor_shows_store_size` checks the exact line format for a home with a stopped server and for one with a running server.
- [ ] Implement it. Read the MANIFEST the way `atticdb-tape` does, through a public read-only function if one exists. Otherwise add a small `pub fn stored_segment_size(path) -> Result<u64>` in `atticdb-tape`.
- [ ] Update the docs. D106 records:
  - the 16 MiB floor and the reason for it
  - that the stored size wins
  - that homes get 16 and servers keep 64
  - that existing homes keep 64 MiB and nothing migrates them
- [ ] Run the verify list.
- [ ] Commit: `feat(doctor): store size line; docs and D106`.

## What this plan does not do

- No migration that shrinks an existing store. `atticdb verify` and `export` stay the way to move data.
- No change to the LSN-ROUTES or LSN-OFFSETS files, which are 1 MiB each.
- No sparse-file tricks: preallocation stays real, because the disk-full floor depends on it.
