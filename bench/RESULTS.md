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

Reviewed measurements were recorded on 2026-10-18 on `feat/m2-sim-g1`.
The review base is `a27f307`. Commit identities for each measured stage appear below.
Nothing was pushed.

## Hardware and durability

This run uses the same Apple M4 and internal Apple SSD as M1.
macOS is 27.0.1, build 26A434. The workspace uses APFS, with about 61 GiB free.
The compiler is rustc 1.99.1, commit `58cf8f9c3`.
Cargo is 1.99.1, commit `5f94df478`.
Release settings remain opt-level 3, thin LTO, and 1 codegen unit.
Rust's macOS `sync_data` invokes `F_FULLFSYNC`.
The benchmark uses real filesystem I/O. SimDisk is not used for these measurements.
This is a shared development machine, not an isolated performance host.
Short targeted tests and builds sometimes overlapped the diagnostic stage runs.
The final 1M-turn repetitions run after all test workloads finish.

The requested macOS target is median paired Engine/bare durable-ack p99 at most 1.5x.
**macOS verdict: FAIL. Final median ratio is 1.640x, with a 1.566x through 1.645x spread.**
**Linux NVMe G1 and nightly LazyFS crash validation remain pending.**
This harness is an append-only M2 proxy. It omits cache writes until M3.
It does not yet implement the architecture's complete mixed-cache-write workload.

## Matched trace

```sh
./bench/g1.sh
G1_TURNS=192000 G1_REPS=3 G1_HISTORY=0 ./bench/g1.sh
```

The default trace contains 1,000,000 timed turns per path, with 4 turns per durable batch.
Each measurement runs 3 interleaved repetitions: bare, Tape, Engine, then repeats that order.
The final history experiment runs after all 9 timed phases.
Every summary reports median and min/max across repetitions.
Engine/bare ratios are paired within repetitions before computing their median.
The ratio median is not the quotient of separately selected p99 medians.
All paths use 16 writers. The engine uses 16 tenants and 1024 sessions.
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

Bare and Tape share the same adaptive group collector starting at C2.
Its default timeout is 200 us. Zero disables waiting. Configuration above 200 us is rejected.
The target is the largest of the previous 64 groups.
Actual group boundaries still depend on scheduling and caller preparation.
All paths report observed group sizes. Engine counts exclude registration warmup.
Segment logical preallocation is 64 MiB on every path.
Physical preallocation was skipped. M1's `set_len` path remains unchanged.
No libc dependency or unsafe FFI was added.
Later segment rolls remain inside timing. Initial segment setup and final shutdown stay outside.
Only the engine runs window readers.
Every session's final sequence is checked after reopen, outside timing.

Caller latencies use nearest-rank p50, p99, and p99.9 across 250,000 durable batch acknowledgements.
These are batch latencies, not amortized per-turn latencies.
Throughput reports turns per second across the complete timed phase.
Window latency samples every 64th completed query, while counting every query.

## Ordered 192k-turn experiments

Each cell is median [minimum, maximum] across 3 interleaved repetitions.
The p99 columns use milliseconds per durable batch.
Every stage uses 192,000 turns per path per repetition. Each path has 48,000 latency samples.
These are diagnostic comparisons on a shared host, not isolated causal measurements.
The baseline was rerun with repetitions. The earlier single-run comparison is withdrawn.

| Stage | Commit | Bare p99, ms | Tape p99, ms | Engine p99, ms | Paired Engine/bare ratio |
|---|---|---|---|---|---|
| Review baseline | `863be52` | 10.008 [9.899, 10.818] | 11.989 [10.527, 13.085] | 17.903 [15.808, 21.953] | 1.655 [1.597, 2.193] |
| C1: wake coalescing | `4795ede` | 9.875 [9.100, 10.110] | 13.448 [10.170, 15.947] | 11.689 [11.608, 16.991] | 1.285 [1.176, 1.681] |
| C2: grouping + entropy handle | `753130c` | 6.882 [5.802, 6.993] | 11.889 [9.997, 14.763] | 12.634 [10.921, 15.411] | 1.882 [1.807, 2.239] |
| C3: buffered saves + install chunks | `83fd3db` | 6.092 [5.949, 7.924] | 10.396 [9.997, 12.197] | 13.821 [12.819, 13.988] | 2.104 [1.765, 2.323] |
| C4: sequence receipts | `0888163` | 6.161 [5.947, 8.374] | 10.219 [9.991, 11.057] | 12.867 [12.119, 12.993] | 2.109 [1.447, 2.164] |
| C5: superblock helper | `29c5f48` | 6.027 [5.952, 6.169] | 10.095 [10.080, 14.066] | 13.293 [12.958, 14.078] | 2.234 [2.101, 2.336] |
| C6: keyless digest + borrowed parser | `183f556` | 6.048 [5.951, 8.616] | 11.014 [9.995, 11.991] | 11.965 [11.941, 12.987] | 2.007 [1.389, 2.147] |
| C2 follow-up: batched entropy read | `120c60d` | 6.025 [6.002, 6.143] | 9.943 [9.877, 9.991] | 12.036 [12.018, 12.067] | 1.998 [1.964, 2.002] |
| C2 follow-up: drain ready tail | `0bc83ed` | 5.988 [5.971, 6.786] | 7.476 [6.868, 7.842] | 9.125 [8.054, 9.774] | 1.345 [1.345, 1.637] |

C1 completed 8 checkpoint saves in each 192k repetition, consistent with the byte threshold.
The regression queues 100 over-threshold publications and requires exactly 1 pending wake.
Another regression services stale wakes and requires exactly 10 saves for 100 byte increments at threshold 10.
The former wake queue could hold 16 redundant requests and save after each one.

C2 changes the bare denominator too. Its p99 falls as group formation improves.
A larger Engine/bare ratio between C1 and C2 is not evidence that Engine itself became slower.
The entropy follow-up replaces 32 reads with 1 in the 32-record regression.
Caller hashing runs after releasing the shared entropy handle.

C3's checkpoint-write regression went from 22 writes to 1 for 20 session pages.
Buffering trades checkpoint-sized memory for fewer I/O calls, outside the resident slot budget.
The review identified install holds that the former snapshot-only counter could not see.
The new State-hold metric includes snapshots, each 32-page install chunk, and final watermark publication.
C3's largest observed State hold across its 3 repetitions was 608.791 us.
Old-file unlink and directory sync no longer hold State.

C4's read-fault regression failed before the fix and now leaves the fault unused after append and retry.
Prepare stages sequence metadata. Sync and publication precede releasing that opaque receipt.
Checkpoint retry entries persist it, while full replay reconstructs it from committed turns.
Older derived checkpoint records without receipts fall back to replay. The tape format is unchanged.

C5's regression previously observed `zerodb-writer` writing periodic superblocks.
It now requires `zerodb-superblock`.
The helper samples only the published durable boundary.
Open still installs synchronously. Shutdown joins the helper before its final synchronous install.
Metadata failures poison later requests, without invalidating earlier durable acknowledgements.

C6 skips retry hashing without a key and computes keyed retry digests on callers.
Its parser validates borrowed slices instead of copying turn content during prepare and publish.
The first turn is decoded once during prepare.
Full tape integrity refresh after sequence mutation remains on the writer.
It cannot reuse the caller's old payload digest without changing or weakening the checked bytes.
No digest or durability check was removed.

### Gap diagnosis before the timeout-drain follow-up

The entropy-only 192k follow-up still misses 1.5x.
Its paired ratio is 1.998 [1.964, 2.002], despite fixing the reviewed paths.
Bare groups average 15.989 through 16.000 requests.
Tape groups average 13.849 through 14.829. Engine groups average 9.930 through 10.492.
Therefore the bounded coalescer improves formation but does not eliminate split groups under engine reader load.
Requests missing the 200 us window still wait another durable sync.
Increasing that bound would violate this review's limit.
The median throughput ratio is 10,161.713 / 16,219.386, or 0.627.
The median group-size ratio is 9.969 / 15.995, or 0.623.
The visible sync-amortization deficit closely matches the throughput deficit.
This does not isolate every pre-enqueue CPU or scheduling contribution.

This exposed another grouping bug.
When scheduling exhausted the wait budget, the collector returned without draining requests already queued.
That ready tail unnecessarily waited for another sync.
The deterministic regression returned `[1]` instead of `[1, 2, 3]` before the fix.
Commit `0bc83ed` adds a final nonblocking drain and records the resulting full group size.
It does not extend the blocking wait beyond 200 us.
Bare and Tape both use the corrected collector.
A partial 1M run was cancelled and discarded after finding this issue.

### Corrected timeout drain

The corrected 192k experiment restores Engine groups to 15.995 requests in all 3 repetitions.
Tape groups range from 15.968 through 15.989. Bare groups range from 15.989 through 16.000.
Engine throughput median is 15,920.939 turns/s, versus bare's 16,078.143 turns/s.
The paired p99 ratio is 1.345 [1.345, 1.637].
That meets the requested median target in this diagnostic run, but 1 repetition still exceeds it.
This is not a substitute for the final 1M-turn verdict.

Bare preframes fixed transactions, while Tape still hashes, frames, maintains chains, and handles segment metadata.
Engine additionally publishes session slots, checkpoints, and competes with window reads.
Moving superblocks off the writer removes writer stalls, not the SSD's physical flush work.
These remaining costs must not be hidden by acknowledging before sync or replying before publication.
The aggregate harness does not isolate their individual CPU and I/O contributions.
The final 1M-turn repetitions below determine the requested local verdict.

## Final 1M-turn repetitions

Measured at `0bc83ed`, including the corrected nonblocking ready-tail drain.
All required validation finished before this run started. No test workloads from this task overlapped it.
The command completed with status 0:

```sh
G1_TURNS=1000000 G1_REPS=3 G1_HISTORY=1 ./bench/g1.sh
```

Every timed path appends 1,000,000 turns in each repetition.
That is 3,000,000 timed turns per path, with 250,000 durable-ack samples per repetition.
Every engine reopen verified the expected final sequence in all 1024 sessions.

| Repetition | Bare p99, ms | Tape p99, ms | Engine p99, ms | Paired Engine/bare ratio |
|---:|---:|---:|---:|---:|
| 1 | 6.009 | 7.110 | 9.856 | 1.640 |
| 2 | 7.589 | 9.926 | 11.885 | 1.566 |
| 3 | 6.085 | 7.800 | 10.011 | 1.645 |
| Median [min, max] | 6.085 [6.009, 7.589] | 7.800 [7.110, 9.926] | 10.011 [9.856, 11.885] | **1.640 [1.566, 1.645]** |

The following cells are median [minimum, maximum] across repetitions, not pooled sample quantiles.

| Path | Turns/s | Ack p50, ms | Ack p99, ms | Ack p99.9, ms |
|---|---|---|---|---|
| Bare | 16,132.7 [15,787.5, 16,613.8] | 3.968 [3.951, 4.035] | 6.085 [6.009, 7.589] | 12.139 [11.956, 13.028] |
| Tape | 15,756.8 [13,197.3, 15,773.9] | 3.994 [3.992, 4.426] | 7.800 [7.110, 9.926] | 20.080 [19.923, 28.147] |
| Engine + readers | 14,493.7 [13,497.0, 14,572.8] | 4.043 [4.036, 4.247] | 10.011 [9.856, 11.885] | 21.072 [21.049, 22.194] |

### Final gap diagnosis

All final repetitions exceed 1.5x. The local target is not met.
The 192k median pass did not carry over to the longer trace.
The corrected collector now forms nearly full groups on every path:

| Repetition | Bare requests/group | Tape requests/group | Engine requests/group | Checkpoint saves | Max State hold, us |
|---:|---:|---:|---:|---:|---:|
| 1 | 15.997 | 15.987 | 15.997 | 45 | 798.792 |
| 2 | 15.999 | 15.985 | 15.998 | 45 | 1406.833 |
| 3 | 16.000 | 15.981 | 15.998 | 45 | 1463.750 |

Thus split groups no longer explain the final gap.
The engine completes 45 checkpoints per run, rather than the review's reported 553 saves.
This matches the byte threshold for the 3.036 GB encoded trace.
All runs end with 5 checkpoint files. Their sampled watermarks are 997,336, 997,332, and 997,320.
State-hold maxima include installation. The snapshot is the maximum observed hold in these runs.
The longest reported hold is 1.464 ms.
The former snapshot-only counter missed install holds reported at 12 ms.

The remaining measured gap is primarily in the tail.
Median p50 is 4.043 ms for Engine and 3.968 ms for bare.
Median p99 is 10.011 ms versus 6.085 ms.
Tape's own median p99 is already 7.800 ms, before engine publication and reader load.
The extra work therefore exists in both the tape layer and the engine layer.

Bare uses preframed bytes. Tape adds framing, integrity work, chains, and manifest installs when segments roll.
Engine also adds session publication, checkpoints, and concurrent windows.
The 45 checkpoints require 180 file/directory sync calls, plus 5 compaction cleanup directory syncs.
Periodic superblocks add synced atomic installs even though their I/O no longer runs on the writer.
On this machine those file syncs use `F_FULLFSYNC`; moving them elsewhere does not remove media work.
The aggregate harness does not separate CPU cost from those competing durability barriers.
It does show that wake amplification, read-after-ack, and half-group formation have been removed.
No durability, integrity, or publication guarantee was relaxed to chase the target.

Further profiling must attribute per-phase CPU, sync time, metadata barriers, and reader contention.
Linux NVMe measurements remain necessary. This macOS failure is not a Linux verdict.

### Concurrent window readers

The readers completed 1,191,295 windows across the 3 engine phases.
Latency samples use stride 64:

| Repetition | Windows | Samples | p50, us | p99, us | p99.9, us |
|---:|---:|---:|---:|---:|---:|
| 1 | 388,984 | 6078 | 326.041 | 966.458 | 3180.833 |
| 2 | 415,072 | 6486 | 327.333 | 1027.834 | 1831.167 |
| 3 | 387,239 | 6052 | 328.542 | 914.584 | 1912.583 |

## Window work versus history

Each cell measures 2000 queries after 100 warmups, returning exactly 16 turns.
Content is fixed at 256 B for this separate history-length experiment.
Since queries start near the middle, except at sequence 1 for the 16-turn history.
Both query forms issue exactly 16 tape reads at every history length.
The harness asserts that count, rather than inferring complexity from elapsed time.

| History turns | Query | Returned | Tape reads/query | p50, us | p99, us | p99.9, us |
|---:|---|---:|---:|---:|---:|---:|
| 16 | Last 16 | 16 | 16 | 237.625 | 365.375 | 503.959 |
| 1024 | Last 16 | 16 | 16 | 232.375 | 364.709 | 476.333 |
| 65,536 | Last 16 | 16 | 16 | 233.917 | 380.250 | 428.375 |
| 16 | Since, limit 16 | 16 | 16 | 242.458 | 351.583 | 432.458 |
| 1024 | Since, limit 16 | 16 | 16 | 233.375 | 350.750 | 420.958 |
| 65,536 | Since, limit 16 | 16 | 16 | 232.792 | 370.041 | 427.000 |

History grows 4096x while record-read work remains flat.
Tail latency still reflects filesystem and scheduler noise.

## Engine simulation and validation

The simulator drives the real Engine API on SimDisk and SimClock.
The `sim` feature's inline option disables background worker threads.
Fixed salts and seeded checkpoint points remove scheduler and entropy variance.
Production defaults retain worker threads and OS entropy.

Each seed runs 120 randomized batches across 48 scopes, with 4 tenants and 2 agents.
It includes 3 append crash generations and cuts during checkpoint installation.
Sync failures occur before persistence and after persistence. Each failure triggers live reopen.
Half the seeds use a 4 KiB hot-page budget.
Each generation corrupts a checkpoint and checks full replay with a warning.
Corruption targets a page-record checksum byte while leaving the file magic intact.
Keyless batches use unique oracle metadata rather than requiring an idempotency key.
After a mid-append cut followed by success, a second post-return image must contain the acknowledgement.
The independent oracle scans recovered tape records before opening the engine.
It verifies dense sequences, exact bytes, stable tenant IDs, and complete transactions.
It also checks last/since windows and original idempotent sequence, transaction, and LSN receipts.
Retries cover uncertain results, live reopen, and crash recovery.
Conflicting retries must write nothing and report `NotCommitted`.
A repeat-run test compares the complete final histories for the same seed.
Every seed also runs 3 grouped generations on the real Engine inline queue.
Each step queues 2 through 4 distinct appends, a same-group retry, and a conflict.
Seeded I/O callbacks call checkpoint save between prepare and publish.
The byte-threshold pending flag is serviced through the same wake check as production.
Grouped sync failures and crashes check complete uncertain transactions and stable retry receipts.
The dedicated group test requires 3 requests, 2 writes, and 1 sync before returning their sequences.
Negative oracle tests reject partial uncertain batches and missing acknowledged records.

The earlier 10,000-seed run covered the pre-review simulator, not these added grouped scenarios.
The reviewed code is validated separately below.
The expanded 2,000-seed engine run passed in 124.49 seconds.
The 2,000-seed M1 run passed in 17.99 seconds.
No storage correctness failures occurred in either run.
CI now runs the default 500 engine seeds in release mode on Linux and macOS.

| Command | Result |
|---|---|
| `cargo fmt --all --check` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --workspace` | Passed. Default 500 M1 seeds. Release-only stress ignored. |
| `cargo test --workspace --release` | Passed. Default 500 M1 seeds and release stress. |
| `ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-tape --release --features sim --test sim` | Passed. Seeds 0 through 1,999. |
| `ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-core --release --features sim --test engine_sim` | Passed. Seeds 0 through 1,999. All 4 tests passed. |
| `cargo check -p zerodb-core --lib --no-default-features` | Passed. Exposed a deterministic-only method warning, fixed in `a139813`. |
| `cargo clippy -p zerodb-core --lib --no-default-features -- -D warnings` | Passed after that feature-gating fix. |
| `cargo bench -p zerodb-core --bench g1 --no-run --quiet` | Passed |
| `cargo bench -p zerodb-tape --bench append --no-run --quiet` | Passed |
| `G1_TURNS=192000 G1_REPS=3 G1_HISTORY=0 ./bench/g1.sh` | Passed after each ordered stage, including the entropy follow-up. |
| `G1_TURNS=1000000 G1_REPS=3 G1_HISTORY=1 ./bench/g1.sh` | Passed. All 9 timed phases, reopen checks, and history checks completed. Local performance verdict: FAIL. |

The full validation chain exited with status 0.
It was rerun after the ready-tail correction, before the final benchmark.
Both clippy configurations completed without warnings.

## Review commits

Every commit is signed off and carries the Copilot co-author trailer.

| Commit | Change |
|---|---|
| `863be52` | Three interleaved repetitions and paired ratio summaries. |
| `4795ede` | Coalesced checkpoint pending flag and threshold recheck. |
| `753130c` | Bounded adaptive groups and persistent entropy handle. |
| `83fd3db` | Buffered checkpoint writes, chunked installs, and full State-hold metric. |
| `0888163` | Opaque sequence receipts released after publication. |
| `29c5f48` | Periodic superblock helper with ordered shutdown join. |
| `183f556` | Keyless digest bypass, caller retry hashing, and borrowed payload parsing. |
| `15afc78` | Grouped simulation, in-step checkpoints, post-ack images, keyless batches, and page CRC damage. |
| `120c60d` | One entropy read per caller batch. |
| `e27b43f` | Correct medians for even repetition counts. |
| `cab2a4c` | Documented execution, simulation, and benchmark paths. |
| `a139813` | Gate the deterministic superblock method out of production-only builds. |
| `0bc83ed` | Drain already-ready arrivals when the bounded group wait ends. |

Core has 2343 physical source lines before trailing unit-test modules.
Of those, 2166 are nonblank and noncomment lines.
These counts include feature-gated simulation entry points and inline test instrumentation.
They are not a production-only size measurement. The previous pre-review line count is withdrawn.
The engine simulator contains 769 lines.
Linux NVMe measurements and LazyFS testing remain future work.
