# M1 append baseline

Measured on 2026-10-18. These are local development numbers, not G1 results.

## Hardware

- CPU: Apple M4.
- OS: macOS 27.0.1, build 26A434.
- Disk: internal APPLE SSD AP0512Z, 500.28 GB, Apple Fabric, APFS.
- Workspace volume: 460 GiB, about 64 GiB free during measurement.
- Compiler: rustc 1.99.1, commit `58cf8f9c3`.
- Cargo: 1.99.1, commit `5f94df478`.
- Criterion: 0.5.1. Release settings: opt-level 3, thin LTO, 1 codegen unit.

Hardware commands:

```sh
sysctl -n machdep.cpu.brand_string
sw_vers
diskutil info /
system_profiler SPNVMeDataType
df -h .
rustc --version
cargo --version
```

## Method

```sh
cargo bench -p zerodb-tape --bench append -- --noplot
```

The recorded run had no concurrent test workload.
An earlier run overlapped filesystem stress. Its numbers aren't included.
This was a shared development machine, not an isolated performance host.

Each path preallocates 64 MiB segments using `set_len`, followed by file and directory sync.
Initial preparation is outside measurement. Any later roll remains inside measurement.
The bare path calls `write_all` and `sync_data` for each encoded transaction.
Both paths use identical content and encoded byte counts, including salts, headers, and the transaction footer.
Encoded sizes are 524 B for 256 B content, and 4,364 B for 4 KiB content.
The bare frame repeats fixed metadata. Tape generates fresh salts, LSNs, timestamps, and tenant chains.
Tape also includes caller preparation, queueing, acknowledgements, and periodic superblock maintenance.

Criterion uses 20 samples, 1 second of warmup, and a 3-second target measurement.
The mean comes from `target/criterion/**/new/estimates.json`, not the displayed regression slope.
For 16 writers, each Criterion iteration contains 16 appends, 1 per writer.
The amortized mean divides the round mean by 16. It isn't caller latency.
The single-writer Tape measurement uses the same threaded helper with 1 caller.
Thread creation precedes timing. Final joins remain inside the round measurement.

A separate manual loop records 1,024 append latencies per case.
For 16 writers, each caller contributes 64 samples.
The table reports nearest-rank p50 and p99 from these individual caller timings.
Manual latency and Criterion means come from separate loops.

## Numbers

| Content | Path | Writers | Criterion mean, ms/round | Amortized mean, us/op | p50, ms/op | p99, ms/op |
|---|---|---:|---:|---:|---:|---:|
| 256 B | Bare | 1 | 3.982 | 3,982.308 | 3.993 | 4.323 |
| 256 B | Tape Durable | 1 | 4.017 | 4,016.876 | 3.994 | 5.067 |
| 256 B | Tape Durable | 16 | 7.284 | 455.236 | 7.915 | 8.647 |
| 4 KiB | Bare | 1 | 3.515 | 3,515.329 | 3.889 | 5.050 |
| 4 KiB | Tape Durable | 1 | 3.593 | 3,593.468 | 3.946 | 5.043 |
| 4 KiB | Tape Durable | 16 | 8.635 | 539.715 | 6.883 | 8.264 |

On macOS, Rust's `sync_data` uses `F_FULLFSYNC`.
These numbers aren't comparable to Linux `fdatasync` numbers.
G1 requires Linux NVMe. No G1 performance claim follows from this run.
The 1,024-sample p99 estimates remain sensitive to scheduler and storage noise.

## Validation

The benchmark numbers above were recorded at `822cf91`, before the final M1 review.
The reviews change recovery, clean shutdown, and API visibility, not the measured append path.
The benchmark target is rebuilt after review. The timing loops aren't rerun.
The following review validation completed on 2026-10-18.

| Command | Result |
|---|---|
| `cargo fmt --all --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed |
| `cargo check --lib --no-default-features` | Passed. Production builds without simulation access. |
| `cargo test --workspace` | Passed. 500 seeds. Stress ignored in debug. |
| `cargo test --workspace --release` | Passed. 500 seeds and 20,000 appends across 16 threads. |
| `ZERODB_SIM_SEEDS=10000 cargo test --release --features sim --test sim -- --nocapture` | Passed. Seeds 0 through 9,999. |
| `cargo test --doc` | Passed. Private implementation import paths fail to compile as expected. |
| `cargo bench --bench append --no-run` | Passed. Benchmark compiles against the narrowed API. |
| `cargo bench -p zerodb-tape --bench append -- --noplot` | Passed before review at `822cf91`. All 6 cases recorded above. |

The final validation chain exited with status 0.
The required commands were rerun after the short-session fix at `c9d1909`.
Both workspace suites passed their default 500 seeds.
The 10,000-seed release run passed in 75.15 seconds.
Release stress passed 20,000 appends across 16 threads in 10.55 seconds.
Every randomized seed runs 2 or 3 crash generations.
The 10,000-seed run includes 910 deliberate below-superblock corruption cases.
Each required `CorruptionDetected`. All other recovery opens succeeded.
It also includes 104 supplemental large-record scenarios.
The main workload still uses 64 KiB segments for every seed.
The normal and release suites both exercise the oracle's negative tests.
These tests reject partial uncertain transactions and changed uncertain content.

Replay commands:

```sh
ZERODB_SIM_SEED=42 cargo test --features sim --test sim -- --nocapture
ZERODB_SIM_SEEDS=10000 cargo test --release --features sim --test sim
cargo test --release --test stress
```

## Open cost limit (M2)

Open remains O(tape) for CRC, digest, and chain verification.
Recovery verifies every segment before application replay, even with a current checkpoint.
The replay counter measures records above the checkpoint watermark, not total open work.
A later milestone must add verified-segment markers before we claim checkpoint-bounded open time.
The append measurements above don't measure startup latency.

## Size budget

Production source measures 2,471 lines, including blank lines and comments.
The count excludes `src/sim.rs` and trailing `#[cfg(test)]` modules.
It includes the always-exported SimDisk implementation.
The raw count includes every `src/*.rs` line: 3,198.
That includes 536 simulation-only helper lines and 191 embedded unit-test lines.
Production remains below the M1 budget of 3,000.

```sh
wc -l crates/zerodb-tape/src/*.rs
python3 - <<'PY'
from pathlib import Path
total = 0
for path in Path("crates/zerodb-tape/src").glob("*.rs"):
    if path.name != "sim.rs":
        total += len(path.read_text().split("#[cfg(test)]")[0].splitlines())
print(total)
PY
```

## Fixes and limits

The carry-over regression reproduced a crash after manifest trimming but before orphan unlink.
Open now removes later segment files before recovery and syncs the directory.
An additional seeded regression found incomplete retry durability after cleanup's directory sync failed.
Seed 2 reproduced the orphan returning after a successful retry.
Open now syncs the directory even when no orphan remains visible.
Both regressions failed before their fixes. Each fix has its own signed-off commit.
The carry-over fix is `643ec6d`. The retry regression fix is `f92ca69`.

### Final review fixes

The initial harness missed process restart without power loss after failed sync.
The reviewer's sequence reproduced lost durable acknowledgements at seed 0 before the fix.
Recovery now rewrites the retained suffix above the superblock, then syncs its files and directory.
This re-dirties readable cache bytes left clean after failed writeback.
The regression passes 200 seeds in both stale and advanced superblock cases.
A separate regression covers suffix rewriting across rolls and before a trimmed tail.

The rewrite includes zero padding and the last segment's unused preallocation.
A further regression reproduced failed tail-repair retry losing a later rolled acknowledgement at seed 1.
It passes 32 seeds with both stale and advanced superblocks after rewriting padding.
The rewrite covers data written since the last superblock install, plus anything unsynced at a crash.
Under continuous load, that's at most about 1 second of writes.
Delayed superblock maintenance extends that window.
The physical rewrite also includes roll padding and at most 1 final segment's unused capacity.

All recovery error LSN increments now saturate.
A CRC-valid transaction at `u64::MAX`, followed by an oversized segment, reproduced the overflow before the fix.
Its regression checks both zero and maximum superblock values.

Simulation now includes live reopen after poison, followed by more acknowledgements and a power-loss crash.
Some generations also capture a crash during recovery's rewrites, zero-tail repair, or manifest cleanup.
Corruption samples random byte offsets across all segments through the superblock transaction's footer.
The selected bit spans 0 through 7.
A coverage test checks both segments, offsets beyond the first header, and every bit.
The SimDisk regression confirms live reopen preserves failed cached bytes.
Another sync doesn't heal them. Rewriting identical bytes does.

Production exports storage types from the crate root, including `TapeOptions`.
Directory, recovery, segment, writer, and codec modules are crate-private.
The `sim` feature gates external access to `WriterCore`, `DataDir`, and other deterministic helpers.
Development self-dependency enables that feature for tests and benches.
Default library builds don't expose it.
`deny(missing_docs)` enforces public API documentation.
The root API test and compile-fail documentation tests verify the supported and rejected import paths.

| Commit | Change |
|---|---|
| `4728f9a` | Redirty recovered suffixes. Add live-restart regressions and generations. |
| `e8ec017` | Saturate corruption LSN reporting at `u64::MAX`. |
| `c4a6f08` | Spread corruption across durable prefix bytes and all bits. |
| `c8f1008` | Narrow and document the public API. Gate deterministic access. |
| `5c552db` | Persist cached zero padding. Cover failed repair retry and recovery cuts. |
| `c9d1909` | Checkpoint recovery and clean shutdown. Bound short-session rewrites and skip untouched segment syncs. |

### Short-session re-review

The re-review found short sessions could leave the superblock at 0 indefinitely.
The original regressions failed before the fix.
After 2 sessions, the next open rewrote 2,048 bytes despite only 1,024 bytes written in the previous session.
Corrupting the first short session's acknowledged data also returned success instead of `CorruptionDetected`.
Recovery didn't install its newer durable LSN before returning.

Open now installs any recovered boundary after suffix rewrites, touched-file syncs, and directory sync.
Clean shutdown drains requests, syncs pending Fast records, and forces a checkpoint without the 1-second limit.
Dropping `Tape` joins that shutdown.
Final sync failures and poisoned writers don't advance the boundary.
Unchanged segments fully covered by the superblock aren't synced again.

The counting I/O regression runs 16 short sessions, advancing SimClock by 0.5 seconds between sessions.
Each append fills its 1,024-byte segment, excluding unused-preallocation overhead from the measured bound.
Each open stays within the previous session's write count.
After the first session, no segment data is rewritten or synced during those opens.
A separate 8-session test checks the installed boundary and detects damage to older session data.
Other tests verify recovery sync ordering, final Fast sync before checkpoint, and failed shutdown preserving the old boundary.

The physical rewrite still includes retained padding and at most 1 final segment's unused capacity.
The superblock fix prevents those costs from growing across historical segments.
Drop can't return shutdown errors. The next successful open recovers and checkpoints before accepting writes.

Each review fix is signed off and carries the Copilot co-author trailer.
The simulator models I/O boundaries and torn writes. It doesn't replace real-filesystem crash testing.
The stress test covers real queue contention, not power loss.
Linux NVMe measurements and LazyFS testing remain future work.
