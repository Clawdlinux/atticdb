# 0007: Hot memory without corruption (research)

Owner ask (2026-11-01): a hot memory tier must never corrupt memory. Research designs that shipped in production, and the measured causes of corruption, before choosing.

## What we have today

AtticDB's in-RAM indexes hold LSN pointers, not content. Session pages are vectors of LSNs. Facts are `Rev {lsn, deleted}` in a `BTreeMap`.

Every point read goes back to the tape:
- It re-checks the frame's CRC32C, which is seeded by segment and offset.
- It checks that the frame is the LSN the index asked for. If not, it fails with `LSN index points to another record` (`atticdb-tape/src/writer.rs`).

So a flipped pointer in RAM is reported as corruption. Wrong content is never returned. Because the CRC is seeded by location, a write that lands at the wrong offset fails its check too.

Other defenses already in place:
- A failed sync poisons the engine (0001, "Fsync failure").
- Every index rebuilds from the log (I8).
- Memory is bounded (I12).
- `forbid(unsafe_code)` in every crate except `atticdb-sys`.
- 2 deterministic sims, run on 2,000 seeds in CI and 20,000 nightly.
- SimDisk injects crashes, failed syncs, ENOSPC, sector sizes and bit flips.
- A LazyFS power-loss test runs nightly.

**The risk of a hot tier:** once content bytes live in RAM, the read path no longer goes through the tape's check. A bit flip or a bug in cached bytes would be served as truth. Everything below aims to keep the check.

## Production designs

| System | Hot / cold | In-RAM protection | What went wrong | Source |
|---|---|---|---|---|
| FASTER HybridLog | 1 log in 3 regions: mutable in RAM, read-only in RAM, stable on disk | Read-only region is immutable. Epoch-based reclamation. No per-record checksum in the KV core | Garnet AOF data loss on graceful shutdown (fixed) | [SIGMOD 2018](https://www.microsoft.com/en-us/research/uploads/prod/2018/03/faster-sigmod18.pdf), Garnet #1958 |
| Bitcask (Riak) | RAM keydir of pointers over an append-only log, hint files | Mutex only, no RAM checksum | PULSE found a deleted key coming back after a merge race | [basho/bitcask#180](https://github.com/basho/bitcask/issues/180) |
| RocksDB | Memtable + WAL + block cache, immutable SSTs | Per-key-value protection bytes, added for RAM and CPU faults. Block checksums | A `DeleteRange` compaction bug brought data back | [per-kv checksum, 2022](https://rocksdb.org/blog/2022/07/18/per-key-value-checksum.html) |
| TigerBeetle | Mutable and immutable tables in RAM, a grid of content-addressed blocks on disk | Everything checksummed and hash-chained. Static allocation after startup. 4 superblock copies | VOPR finds bugs before release, with a repro seed | [safety](https://docs.tigerbeetle.com/concepts/safety), [VOPR](https://docs.tigerbeetle.com/internals/vopr) |
| LMDB | One mmap'd copy-on-write B+tree | Live pages are never overwritten. Read-only map by default | `MDB_WRITEMAP` gives up the read-only protection | [lmdb.h](https://raw.githubusercontent.com/LMDB/lmdb/mdb.master/libraries/liblmdb/lmdb.h) |
| SQLite | Page cache, WAL or rollback journal | None for RAM. Recovery replays the journal | Mostly from breaking documented rules: fd reuse, broken locks | [howtocorrupt](https://www.sqlite.org/howtocorrupt.html) |
| Redis | Everything in RAM, RDB fork snapshot, AOF | None for RAM | AOF corrupted mid-file after power loss, not auto-fixable | redis/redis#5177 |
| Kafka | Log segments, mmap'd indexes | Per-record CRC checked on read | Recovery truncates at the first bad batch | `LogSegment.recover()` |

## Measured causes of corruption

| Risk | Evidence | Proven mitigation |
|---|---|---|
| App crash-consistency bugs | 60 found across 11 systems, LevelDB, LMDB and SQLite among them ([OSDI 2014](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai)) | Crash testing on fault-injecting file systems |
| fsync errors swallowed | ext4, XFS and Btrfs mark pages clean after EIO ([ATC 2020](https://www.usenix.org/conference/atc20/presentation/rebello)) | Fail-stop, never retry. PostgreSQL since 2019 |
| Latent and misdirected disk errors | 8.5% of nearline disks over 32 months ([SIGMETRICS 2007](https://research.cs.wisc.edu/wind/Publications/latent-sigmetrics07.pdf)) | End-to-end checksums, scrubbing |
| DRAM bit flips | About 1/3 of machines see a correctable error per year. Uncorrectable: 1.3% of machines per year ([SIGMETRICS 2009](https://www.cs.toronto.edu/~bianca/papers/sigmetrics09.pdf)) | ECC, checksums on in-memory data |
| CPUs that compute wrong | "A few mercurial cores per several thousand machines" ([HotOS 2021](https://sigops.org/s/conferences/hotos/2021/papers/hotos21-s01-hochschild.pdf)). Meta: hundreds of CPUs ([arXiv 2102.11245](https://arxiv.org/abs/2102.11245)) | Check at the endpoint, recompute |
| Memory-unsafe code | About 70% of Microsoft's and Chromium's serious bugs | Safe Rust removes the class. It doesn't stop logic bugs, races or deadlocks. Release builds wrap on integer overflow by default |
| Merge and compaction bugs | Bitcask #180 and RocksDB `DeleteRange`: deleted data came back | Simulation and stress tests over the rewrite path |

The last row matters most for us. In both incidents the hot RAM structure was fine. Deleted data came back through a rewrite of cold data. For AtticDB, that would be forgotten content reappearing, which breaks I10.

## Design this points to

1. **The tape stays the only source of truth.** Every hot structure is a cache that can be rebuilt. On any mismatch, drop it and rebuild from the tape. Never repair it in place.
2. **Hot bytes are immutable, verified frames.** Cache the durable frame bytes with their stored CRC. Re-check the CRC and the LSN on every serve, as the tape read does today. This is RocksDB's per-record protection, using the checksum we already store. CRC32C runs in hardware, at GB/s. Nothing in the cache changes in place. An eviction drops the entry, and the next read refills it from the tape. This is FASTER's read-only region, minus its mutable region: our records are already durable before anyone can read them.
3. **The cache is a fixed-size arena.** One allocation at startup, sized by config, with no growth (I12). In safe Rust this isn't about allocator bugs. It makes memory use predictable and rules out an out-of-memory crash under load, TigerBeetle-style.
4. **Admit the live set only.** Admit the recent window of each session, current facts and file refs. Tool outputs are 57% to 92% dead tokens, so they enter only when read.
5. **Compress at write time, not by rewriting later.** A frame flag compresses a turn when it's appended. Cold data is then small with no compaction pass, which removes the Bitcask #180 and RocksDB `DeleteRange` failure mode. This needs a codec. Rust's standard library has none, so it breaks the "no new crates" rule. Owner decision.
6. **Derived views are caches too.** The prebuilt context block and the search index carry the LSN they cover and a checksum. They're checkpointed like Bitcask hint files and rebuilt from the tape on any mismatch.

## Gaps to close first, independent of the design

- `overflow-checks = true` in `[profile.release]`. Today offset and length arithmetic wraps silently in release builds.
- Fuzz the frame, checkpoint and HTTP decoders, in a separate fuzz crate that never ships.
- Run Miri on `atticdb-sys`, the only crate with `unsafe`.
- SimDisk faults to add: misdirected writes, lost writes that report success, and EIO on read. The location-seeded CRC should catch the first two. No test proves it yet.
- Scrub while the server runs. Today the full CRC scan happens only at open and in `atticdb verify`. A server up for weeks never re-checks old segments.
- Publish `docs/how-to-corrupt.md`, as SQLite does. It covers the conditions we can't defend against: 2 processes on one data dir without the lock, synced folders, network file systems, and RAM without ECC.

## Unverified

- Exact silent-CPU-error rates. Meta and Google withhold them.
- FoundationDB Redwood internals. Its source wasn't read directly.
- Kafka's fuzzing and simulation practice.
