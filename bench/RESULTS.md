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

# M2 engine simulation and G1

Measured on 2026-10-18 at `0e9b951`, based on `60293c9`.
The engine simulator is `50f933a`. The matched benchmark harness is `0e9b951`.
These commits are local on `feat/m2-sim-g1`. Nothing was pushed.

## Hardware and durability

This run uses the same Apple M4 and internal Apple SSD as M1.
macOS is 27.0.1, build 26A434. The workspace uses APFS, with about 61 GiB free.
The compiler is rustc 1.99.1, commit `58cf8f9c3`.
Cargo is 1.99.1, commit `5f94df478`.
Release settings remain opt-level 3, thin LTO, and 1 codegen unit.
Rust's macOS `sync_data` invokes `F_FULLFSYNC`.
The benchmark uses real filesystem I/O. SimDisk is not used for these measurements.
This is a shared development machine, not an isolated performance host.
No test or simulation workloads from this task overlapped the recorded benchmark.

**G1 verdict: pending. Linux NVMe and nightly LazyFS crash validation are still required.**
macOS has no G1 pass threshold. A local ratio is not a Linux performance claim.

## Matched trace

```sh
./bench/g1.sh
```

The default trace contains 1,000,000 timed turns, with 4 turns per durable batch.
There are 16 writers, 16 tenants, and 1024 sessions.
Content sizes cycle through 256 B, 512 B, 1 KiB, 4 KiB, and 8 KiB.
The engine also runs 2 concurrent last-16 window readers.
Initial tenant and session registration uses 1024 untimed turns.
The timed trace excludes tenant creation. Retry keys are disabled for this performance trace.
Default engine checkpoints remain enabled.

All 3 paths use identical content-size ordering, turn payload lengths, and transaction batching.
The bare baseline reuses M1's frame encoder, preallocation, append, and file sync implementation.
Each encoded turn contains 16 metadata bytes and 63 total codec overhead bytes.
The 5 encoded batch lengths are 6768, 10864, 13936, 14448, and 14704 bytes.
These lengths include salt space, record headers, and transaction footers.
The bare path precomputes fixed metadata and frame bytes outside timing, as in M1.
Tape and Engine include caller preparation, hashing, queueing, and replies.
Engine additionally includes session publication, checkpoints, and concurrent window load.

The bare writer drains queued requests and syncs each group before returning receipts.
Tape uses the same queue-draining group policy. Actual group boundaries depend on scheduling.
The harness reports the bare path's observed batches per sync group.
Segment preallocation is 64 MiB on every path.
Later segment rolls remain inside timing. Initial segment setup and final shutdown stay outside.
Only the engine runs window readers.
Every session's final sequence is checked after reopen, outside timing.

Caller latencies use nearest-rank p50, p99, and p99.9 across 250,000 durable batch acknowledgements.
These are batch latencies, not amortized per-turn latencies.
Throughput reports turns per second across the complete timed phase.
Window latency samples every 64th completed query, while counting every query.

## macOS numbers

The full 1M-turn trace completed. No reduced-N substitution was needed.
All 1024 session tails matched their expected final sequences after reopen.

| Path | Timed turns | Seconds | Turns/s | Ack p50, ms | Ack p99, ms | Ack p99.9, ms |
|---|---:|---:|---:|---:|---:|---:|
| Bare append + sync | 1,000,000 | 78.607 | 12,721.5 | 4.054 | 10.136 | 17.088 |
| Tape Durable | 1,000,000 | 107.602 | 9,293.5 | 6.982 | 17.304 | 32.879 |
| Engine + 2 window readers | 1,000,000 | 174.330 | 5,736.2 | 9.094 | 34.506 | 101.289 |

The bare writer synced 20,481 groups, averaging 12.206 batches per group.
Engine p99 was 3.404x bare p99. Tape p99 was 1.707x bare p99.
The local engine ratio exceeds 1.5x. The Linux-only G1 threshold does not apply to this macOS run.
These numbers do not establish a G1 pass.

Concurrent readers completed 812,792 windows during the engine phase.
The 12,700 sampled windows had p50 384.000 us, p99 1138.125 us, and p99.9 6294.000 us.
The final observed checkpoint watermark was 984,312, with 4 files in its chain.
Maximum measured checkpoint snapshot lock hold was 1269.709 us.
The watermark was sampled before clean shutdown. Open still verifies the complete tape.

## Window work versus history

Each cell measures 2000 queries after 100 warmups, returning exactly 16 turns.
Content is fixed at 256 B for this separate history-length experiment.
Since queries start near the middle, except at sequence 1 for the 16-turn history.
Both query forms issue exactly 16 tape reads at every history length.
The harness asserts that count, rather than inferring complexity from elapsed time.

| History turns | Query | Returned | Tape reads/query | p50, us | p99, us | p99.9, us |
|---:|---|---:|---:|---:|---:|---:|
| 16 | Last 16 | 16 | 16 | 234.750 | 766.958 | 3918.042 |
| 1024 | Last 16 | 16 | 16 | 234.209 | 393.208 | 496.209 |
| 65,536 | Last 16 | 16 | 16 | 235.083 | 437.541 | 574.792 |
| 16 | Since, limit 16 | 16 | 16 | 232.500 | 402.250 | 552.042 |
| 1024 | Since, limit 16 | 16 | 16 | 233.500 | 398.667 | 568.375 |
| 65,536 | Since, limit 16 | 16 | 16 | 232.917 | 404.541 | 500.125 |

History grows 4096x. Last-window p50 stays between 234.209 us and 235.083 us.
Since-window p50 stays between 232.500 us and 233.500 us.
Record-read work is flat. Tail latency still reflects filesystem and scheduler noise.

## Engine simulation and validation

The simulator drives the real Engine API on SimDisk and SimClock.
The `sim` feature's inline option disables writer and checkpoint threads.
Fixed salts and seeded checkpoint points remove scheduler and entropy variance.
Production defaults retain both worker threads and OS entropy.

Each seed runs 120 randomized batches across 48 scopes, with 4 tenants and 2 agents.
It includes 3 append crash generations and cuts during checkpoint installation.
Sync failures occur before persistence and after persistence. Each failure triggers live reopen.
Half the seeds use a 4 KiB hot-page budget.
Each generation corrupts a checkpoint and checks full replay with a warning.
The independent oracle scans recovered tape records before opening the engine.
It verifies dense sequences, exact bytes, stable tenant IDs, and complete transactions.
It also checks last/since windows and original idempotent sequence, transaction, and LSN receipts.
Retries cover uncertain results, live reopen, and crash recovery.
Conflicting retries must write nothing and report `NotCommitted`.
A repeat-run test compares the complete final histories for the same seed.

The 10,000-seed engine run passed seeds 0 through 9,999 in 621.55 seconds.
The separate 2,000-seed engine run passed seeds 0 through 1,999 in 124.00 seconds.
Both ran without storage correctness failures. No bug-fix commits were needed for Group F.
CI now runs the default 500 engine seeds in release mode on Linux and macOS.

| Command | Result |
|---|---|
| `cargo fmt --all --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --workspace` | Passed. Default 500 M1 seeds. Release-only stress ignored. |
| `cargo test --workspace --release` | Passed. Default 500 M1 seeds and release stress. |
| `ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-tape --release --features sim --test sim` | Passed. Seeds 0 through 1,999. |
| `ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-core --release --features sim --test engine_sim` | Passed. 2 tests, including repeat-run equality. |
| `ZERODB_SIM_SEEDS=10000 cargo test -p zerodb-core --release --features sim --test engine_sim` | Passed. 2 tests, including repeat-run equality. |
| `cargo check -p zerodb-core --lib --no-default-features --quiet` | Passed. Normal worker path compiles without simulation. |
| `cargo bench -p zerodb-core --bench g1 --no-run --quiet` | Passed |
| `cargo bench -p zerodb-tape --bench append --no-run --quiet` | Passed |
| `G1_TURNS=1024 ./bench/g1.sh` | Passed. Smoke measurement excluded from reported numbers. |
| `./bench/g1.sh` | Passed. Full 1M turns per path, reopen checks, and fixed-return history experiments. |

## Group F size

Implementation commits add 1114 lines and remove 118 lines across 12 files.
Production Rust changes add 107 lines and remove 16 lines.
The engine simulation is 470 lines. The G1 benchmark is 334 lines.
The shared M1 benchmark helper is 118 lines. The shell entry point is 7 lines.
These counts exclude this results update.

Core source has 2027 physical lines before trailing unit-test modules, including simulation-only blocks.
Excluding `sim`-guarded code and its attributes leaves 1997 production lines, including comments and blanks.
That count is 1837 after removing comments and blank lines.
The count covers `crates/zerodb-core/src/*.rs`, not the earlier Group D tape hooks.
Linux NVMe measurements and LazyFS testing remain future work.
