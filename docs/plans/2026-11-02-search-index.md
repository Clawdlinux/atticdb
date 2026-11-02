# Search word index: plan and results

Owner ask (2026-11-02): no trade-off between compression and search. Find a design that keeps both, then build and benchmark it.

## Idea

Search keeps a record when every query word, or its singular, is a case-insensitive substring of its text. A query word has no whitespace, so a match lies inside 1 whitespace-delimited token. So a dictionary of lowercase tokens, plus the records each token is in, answers the same query exactly:
1. Scan the dictionary for tokens containing the word.
2. Union their records.
3. Check only those records.

The tokens come from plain text at index time, so search inflates only the records it checks.

Measured first, on 96,500 real turns (275 MiB): 634,059 unique tokens, a 13.2 MiB dictionary, and 16.35M postings.

## Gaps closed

| Gap | How |
|---|---|
| False negatives | Tokens come from the text `found()` matches: lossy UTF-8, lowercased as a whole, split on whitespace. `search_with_the_index_equals_a_full_scan` compares every page and cursor with a full walk, on Unicode, final sigma, punctuation, plurals, invalid UTF-8, facts and 2 tenants |
| False positives | Every candidate is read and checked by the same `pick` as a full walk |
| Freshness | Records above the watermark are walked |
| Common words | The newest records are walked first: 1,024 in this change, 64 once trigram token lookup landed (read-path plan). Unselective words are skipped when intersecting |
| Write cost | The builder is a background thread, off the writer |
| Memory | Each token's text is stored once, keyed by hash, with a map for collisions. Budget 512 MiB, then it stops growing |
| Restart | Rebuilt in the background. Search walks what isn't covered yet |
| Simulation determinism | Under `inline_sim` the index catches up inline, before each search |
| Index failure | It stops and reports in `/v1/stats`. Search walks, and the engine is never poisoned |
| Tests reaching the index | `search_walk_first` is an option. Tests use 0 and 7, and the sims rotate 0, 16 and 1,024. Breaking the index on purpose fails both: dropping singulars fails the test, and returning no candidates fails the sims |

## Results

Apple M4, `bench/realload.py` from #147, real Claude Code and Copilot CLI transcripts.

At 512 MiB, 2 interleaved reps:

| Metric | Before | Index | Index + compression |
|---|---|---|---|
| Search p50 | 0.80 to 0.92 ms | 0.83 ms | 1.55 to 1.68 ms |
| Search p99 | 602 ms | 9.8 to 10.2 ms | 19 to 20 ms |
| Disk | 647 MiB | 647 MiB | 327 MiB |
| Fill | 14.2k to 14.3k turns/s | 14.9k turns/s | 15.1k to 15.6k turns/s |
| Index memory | none | 70 MiB | 70 MiB |
| Server RSS | 26 MiB | 122 to 126 MiB | 131 MiB |
| Rebuild after a restart | none | 5.3 s | 7.1 s |

At 2.5 GiB (899,062 turns), 1 rep:

| Metric | Before | Index + compression |
|---|---|---|
| Search p50 | 3.0 ms | 4.5 ms |
| Search p99 | 4,111 ms | 29 ms |
| Disk | 2,786 MiB | 1,186 MiB |
| Fill | 14.5k turns/s | 15.9k turns/s |
| Window of 20, p99 | 1.55 ms | 1.66 ms |
| Index memory / server RSS | none / 50 MiB | 236 MiB / 405 MiB |
| Rebuild after a restart | none | 34 s |

Compression turns on by default (`auto`) with this change, since search no longer pays for it.

## Next

- **Persist the index**, so a restart doesn't scan for 34 s at 2.5 GiB. Write it with checkpoints, tied to the tape by the LSN, txn and commit time of its last record. Drop it on any mismatch or after `recover --truncate-at`.
- **Memory:** more compact postings, or postings on disk, if a real store passes the budget.
- **Ranking:** postings can carry term counts for BM25 (0006).
