# Compress turns at write time: plan

Owner approval (2026-11-02): add 1 vetted, safe-Rust compression crate, then build on autopilot. Research: [0007](../design/0007-hot-memory-without-corruption.md) on #151.

## Dependency check

| Crate | Unsafe | Advisories (RustSec, 2026-11-02) | Deps | Notes |
|---|---|---|---|---|
| `miniz_oxide` 0.9.1 | `#![forbid(unsafe_code)]` | none | `adler2` 2.0.1 (also `forbid(unsafe_code)`) | Rust's std pins this exact version in `library/Cargo.lock`. 1.06B downloads, 488 reverse deps. MIT, Zlib or Apache-2.0 |
| `lz4_flex` 0.14.0 | forbids unsafe only with its safe features | RUSTSEC-2026-0041: decompressing invalid data leaked uninitialized memory | none | Rejected |
| `ruzstd` 0.9.1 | 40 `unsafe` sites | none | none | Rejected: unsafe |
| `snap` 1.1.2 | 24 `unsafe` sites, `build.rs` | none | none | Rejected: unsafe |

Measured on 96,229 real turns (275 MiB) from local Claude Code and Copilot CLI sessions, Apple M4, release build:

| Level | Min turn size | Saved | Compress | Decompress |
|---|---|---|---|---|
| 1 | 256 B | 56.3% | 190 MiB/s | 299 MiB/s |
| 3 | 256 B | 62.1% | 121 MiB/s | 326 MiB/s |
| 6 | 256 B | 62.8% | 80 MiB/s | 328 MiB/s |

Choice: `miniz_oxide =0.9.1`, `default-features = false`, `features = ["with-alloc"]`, in `atticdb-core` only. Raw deflate, level 3, turns of 256 bytes or more. Level 6 buys 0.7 points for 1.5x the CPU.

## Format

**Turn payload version 2.** Same prefix as version 1 (`version, agent, session, seq, role, meta`), then `raw_len: u32` and the deflate stream as a length-prefixed blob, then the retry key as in version 1. `set_seq` is unchanged because the prefix is unchanged.

- A turn is stored as version 2 only when its content is at least 256 bytes and the stream is smaller than the content. Otherwise it stays version 1.
- Decoding is lazy. Replay reads agent, session and seq without inflating. Inflate is bounded: `raw_len` is at most 4 MiB, and the output must be exactly `raw_len` bytes.
- The retry-key digest covers the version 1 encoding. A retry matches its original whether either side was compressed. The fallback digest in `Machine::prepare` canonicalizes too.

**Tape frames don't change.** The frame `flags` field stays 0. A v0.2.0 binary reads nonzero flags as `BadLength`, and recovery truncates on that, so a downgrade could drop data.

**MANIFEST version 3** has the same layout as version 2. It marks a store that may hold turn payload version 2.
- v0.2.0 checks the MANIFEST version exactly. It refuses a version 3 store at open, before it reads any frame.
- A second guard sits behind it: an unknown turn payload version fails replay with `corrupt` and never truncates.

## Rollout and rollback

Config field `compression`: `"auto"` (default), `"on"` or `"off"`.

| Store | auto | on | off |
|---|---|---|---|
| New | MANIFEST 3, compress | MANIFEST 3, compress | MANIFEST 2, plain |
| Existing, MANIFEST 2 | stays 2, plain | upgraded to 3 after recovery, then compress | stays 2, plain |
| Existing, MANIFEST 3 | compress | compress | plain writes, reads all |

- Existing stores change format only when the owner opts in. The upgrade is one-way.
- Rollback: set `"off"`. New writes are plain, and every record stays readable.
- A downgrade to v0.2.0 or earlier is refused at open. Nothing is truncated.
- The MANIFEST upgrade runs after recovery and before the first write, through the same atomic install as today.
- `/v1/stats` reports `manifest_version` and `compressed_turns`, so the migration state is visible.

## Tasks

1. **Tape.** `DataDir` keeps a manifest version and reads 2 or 3. `TapeOptions` gains `manifest_new` and `manifest_min`, default 2 and 2, so today's behavior holds. After recovery, `WriterCore::open_with` raises the version to `manifest_min`. `Tape::manifest_version()` reports it.
   - Test: open, upgrade, reopen. MANIFEST bytes 8..12 read 3, and a version 2 reader rejects it.
2. **Codec.** `turn.rs`: `compress(payload, level, min) -> Vec<u8>`, `canonical(payload)`, and lazy content on `ParsedTurn`.
   - Inline tests: round trip of both versions; incompressible and small turns stay version 1; `set_seq` on version 2; rejects a wrong `raw_len`, `raw_len` over 4 MiB, a bad stream and trailing bytes.
3. **Engine.** `EngineOptions.compression` holds mode, level and min. Both append paths compress after the digest. Map the mode to the tape fields. Add the `compressed_turns` counter.
   - Integration test: write plain, reopen with `on`, write compressed. Read and search both. A retry across the switch deduplicates.
   - Turn compression on in `engine_sim` for half the seeds.
4. **Server.** Parse `compression` in `config.rs`, pass it in `serve.rs`, and add the stats fields.
   - Test: an invalid value is rejected, and stats show the version.
5. **Verify.**
   - Full workspace tests, both sims on 2,000 seeds, clippy on macOS and Windows, fmt.
   - Manual: the released v0.2.0 binary refuses a version 3 store and doesn't truncate it. It still opens an `auto` store that stayed at version 2.
   - Rerun `bench/realload.py` for disk and fill numbers.
6. **Docs.** D95 for the dependency and the format. 0001 MANIFEST note. 0004 Track D status. README config field.

## Not in this change

- Compressing facts and meta. Facts are small, and `about` lines are 1 line.
- Recompressing existing records. No rewrite of old data (0007, rule 5).
- The hot cache and the hardening gaps from 0007. They come next, in separate PRs.

## Results (2026-11-02)

Same Mac, `bench/realload.py` from #147, 512 MiB of real transcripts. 2 interleaved reps per build. Base is the #147 build, whose engine matches `main`.

| Metric | Base | Compressed |
|---|---|---|
| Disk allocated | 647 MiB | 327 MiB |
| Fill | 15,362 to 15,658 turns/s | 16,206 to 16,496 turns/s |
| Batch p99 | 20.8 to 21.1 ms | 14.3 to 14.8 ms |
| Window of 20, p99 | 0.81 to 0.96 ms | 1.14 to 1.22 ms |
| Search, p99 | 184 to 186 ms | 657 to 668 ms |
| Reload after SIGKILL | 0.40 s | 0.42 to 0.43 s |

The first build paced checkpoints by stored bytes. It replayed 24,440 records on reopen against 8,736, and reload took 0.87 s. Pacing by uncompressed bytes fixed it.

The search scan inflates every turn. The index in #117 removes that cost.

Downgrade check with the released v0.2.0 binary (SHA256SUMS verified):
- A new store under `auto` gets MANIFEST 3. v0.2.0 `verify` and `serve` both exit 1, and segment and MANIFEST hashes are unchanged.
- A v0.2.0 store opened by this build under `auto` stays at MANIFEST 2, and v0.2.0 reads every turn after.
- `compression: on` raises it to 3. v0.2.0 then refuses with nothing changed, and this build reads all turns.
