# zerodb

Agent memory database written in Rust. Durable writes and fast recent-history reads.

Status: early. The tape, session engine, S2 disk-full handling, the S3 server and S4 facts are implemented. This isn't ready for production.

## Quick start

```sh
cargo build -p zerodb --release
export PATH="$PWD/target/release:$PATH"
TOKEN=$(zerodb init --config zerodb.json --tenant me)   # 0600 config, token printed once
zerodb serve --config zerodb.json --data-dir ./data &    # TCP 127.0.0.1:7425 and ./data/zerodb.sock
```

Append over TCP, then read the window over the Unix socket:

```sh
curl -s -H "Authorization: Bearer $TOKEN" \
  -d '{"agent":"planner","session":"run-1","idem_key":"k1","turns":[{"role":"user","content":"hi"}]}' \
  http://127.0.0.1:7425/v1/append
curl -s --unix-socket data/zerodb.sock -H "Authorization: Bearer $TOKEN" \
  'http://localhost/v1/window?agent=planner&session=run-1&last=10'
```

Send the same `idem_key` again and you get the same receipt with `"deduplicated": true`.

Save a rule under a key, read it back, and list a prefix:

```sh
curl -s -H "Authorization: Bearer $TOKEN" \
  -d '{"key":"repo/payments/retry-rule","content":"Retry 3 times, then fail.","idem_key":"r1"}' \
  http://127.0.0.1:7425/v1/facts/put
curl -s -H "Authorization: Bearer $TOKEN" 'http://127.0.0.1:7425/v1/facts/get?key=repo/payments/retry-rule'
curl -s -H "Authorization: Bearer $TOKEN" 'http://127.0.0.1:7425/v1/facts/list?prefix=repo/'
```

A later put to the same key replaces its value. `POST /v1/facts/delete` removes it, and `get` then answers `{"fact": null}`.
Add `"if_lsn": <lsn>` to a put to write only if the current value still has that LSN. A mismatch is 409 `conflict`.
Stop the server with `kill %1`. Every acknowledged write is already synced.

For an MCP client, point it at `zerodb mcp`. Use absolute paths. It starts the server if none is running:

```json
{"mcpServers": {"zerodb": {"command": "/path/to/zerodb",
  "args": ["mcp", "--config", "/path/to/zerodb.json", "--tenant", "me", "--data-dir", "/path/to/data"]}}}
```

The model gets five tools: `memory_append` and `memory_recent` for sessions, and `fact_set`, `fact_get` and `fact_list` for keys.
`fact_set` takes `content`, or `delete: true`. The later write wins, so a correction replaces the old rule.

## Keys

Each tenant has one key space. Every agent with the tenant's token sees the same keys.
Agents keep their own keys apart with prefixes, for example `repo/<area>/<topic>` or `agent/<name>/...`.
A prefix is a byte prefix: `repo/pay` matches `repo/payments/...`. End a prefix with `/` for path semantics.
Keys are 1 to 1024 bytes. They travel in JSON bodies and query strings, never in URL paths.

## Sandboxed agents

By default, the harness holds memory. The harness is the trusted loop that calls the model and starts sandboxes.
It runs beside zerodb, reads the window and facts, puts them in the prompt, and appends the results.
Code in the microVM or container gets no token and no socket, so it cannot query zerodb directly.
The harness still picks the session and fact keys itself. `zerodb mcp` lets the model name any session, so do not give it to a model you do not trust with the whole tenant.
Sandboxes can die or be snapshotted. Memory survives and moves between agents, because one zerodb serves them all.

A token grants a whole tenant. Direct access from a sandbox ([sandboxes.md](docs/deploy/sandboxes.md)) is for code you trust with that tenant, or with a tenant of its own.

## Why

We want memory that keeps acknowledged writes and stays fast as history grows.
Session history lives in 1 append-only log. Every index can be rebuilt from that log.
Facts keep one current value per key. Forget and export come in S5. Search is deferred.

The current scope is [0003: slim v0.1](docs/design/0003-slim-v0.1.md).
The [decision log](docs/design/0002-decision-log.md) explains the choices.
[0001](docs/design/0001-architecture.md) retains historical mechanics, not the current feature scope.

## What exists

- `zerodb-tape`: location-bound CRC32C frames, whole-transaction footers, group commit and guarded recovery.
- `zerodb-core`: `append`, `window`, facts with conditional writes, tenant isolation, idempotent retries and incremental checkpoints.
- `SimDisk`: seeded crashes, reordered writes, torn sectors, failed syncs, bit flips and full-disk faults.
- `zerodb serve`: one server per data directory. HTTP/JSON on loopback TCP and a Unix socket, per-tenant tokens. See [crates/zerodb](crates/zerodb/README.md).
- `zerodb stats`: live engine metrics from the Unix socket as `key=value` lines.
- `zerodb load`: closed-loop append load against a server, with ack latency percentiles.
- `zerodb verify` and `zerodb dump`: check a stopped data directory on a copy. Verify names the LSN of damage. Dump lists records without content.
- `zerodb mcp`: a stdio MCP adapter with 5 tools. It finds the running server or starts one, and retries with one key per call.
- A Python client in [clients/python](clients/python/README.md), standard library only.
- Trusted code in sandboxes and microVMs can reach the server through its Unix socket. See [Sandboxed agents](#sandboxed-agents) and [sandboxes.md](docs/deploy/sandboxes.md).

Frame v2 uses a 40-byte header instead of 104 bytes. Its footer payload is 32 bytes.
Record LSNs and transaction IDs are dense. Footers repeat the final record LSN.
Every write waits for sync and publication. There is no lossy acknowledgement mode.
Commit timestamps stay monotonic across restarts, even when the clock moves backwards.

The engine applies `Fact` records. `Forget` has a v1 codec and stays rejected until S5.
An S3 binary refuses to open a data directory that holds facts.
There are no hash chains, signatures or tamper-evidence guarantees.
Someone with write access to the directory can edit history.

### Tape hooks

`Tape::open_with(state_machine, io, clock, path, options)` attaches application state.
`Tape::open(...)` uses `NoopStateMachine`.
The writer runs `prepare` once per batch, in queue order.
`PreparedBatch::records_mut()` allows validated rewrites before framing.
`Prepare::Reject` and `Prepare::Reply` write nothing.

Keep preparation reservations private until `publish`.
Cached replies share the original group's sync and publication outcome.
A failed sync poisons writes and returns `Unknown`. Reads retain the published view.
Publication runs before the durable LSN advances and before replies leave the writer.

Recovery verifies and syncs the tape before ordered `replay` callbacks begin.
Set `TapeOptions::replay_from_lsn` to the loaded checkpoint watermark.
`Tape::scan(from_lsn)` captures a durable boundary and yields application records, without footers.

### Bounds and recovery

`TapeOptions::lsn_index_bytes` bounds resident offset pages and their routing directory. The default is 8 MiB.
Per-segment offset arrays have arithmetic slots. Their routing directory is also paged.
Lookup needs a fixed number of page accesses, regardless of log length.
The derived `LSN-OFFSETS` and `LSN-ROUTES` files are rebuilt at open.

Open verifies the whole tape with CRC32C. Checkpoints reduce application replay, not that full scan.
Open time therefore grows with log size. There is no compaction yet.

Damage below the superblock is `CorruptionDetected`.
Above it, a later validated footer can prove that damaged records were already synced.
Without that proof, recovery truncates and reports the discarded range and byte count.
Damage inside the last synced group can lack a later proof. See I1 in 0003.

`Tape::recover_truncate_at(directory, lsn)` is an explicit operator escape. Close the database first.
Use 0 or the final LSN of a whole transaction.
An interior cut fails with both nearest valid boundaries. The retained prefix must validate.
A durable intent resumes an interrupted cut before normal recovery checks the superblock.
The report and log name the cut, dropped range and byte count.
Dropped LSNs may be reused, including acknowledged LSNs. D73 documents this exception to I3.
`zerodb recover --truncate-at <lsn> --data-dir <dir>` runs it from the command line.

### Session engine

The writer assigns dense session sequences. Windows cost O(returned), not O(history).
Retry keys return original receipts within bounded tenant and global windows.
Receipts set `deduplicated` when a retry key returned an earlier commit, including after reopen.
`Engine::window_budget` caps a window by content and meta bytes. It never cuts a turn and reports `truncated`.
Session pages use a resident-memory budget. Cold pages reload from checkpoints.
A bad checkpoint triggers full replay with a warning.

`Engine::health()` reports `Ok`, `ReadOnly`, or `Poisoned` without checking disk space or blocking.
A clean floor hit rejects the group before preparation. Reads keep serving acknowledged turns.
Only a later write's passing space check clears `ReadOnly`. Poisoning requires reopening.

See [the core API notes](crates/zerodb-core/README.md) for formats and limits.
See [benchmark results](bench/RESULTS.md) for the measured macOS workload and its limits.
See the [head-to-head](bench/H2H.md) against Redis, Valkey, DiceDB and SQLite with an fsync before every ack.

## Steps

| Step | Scope |
|---|---|
| S1 | Frame v2, guarded recovery, bounded tape index. |
| S2 | Disk-full floor and real preallocation. |
| S3 | Shared server, auth, HTTP, MCP and Python client. |
| S4 | Tenant-scoped fact revisions and conditional updates. |
| S5 | Hide requests and markdown export. |
| S6 | Dogfood and measure the remaining gaps. |

## Build and test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --release
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-tape --release --features sim --test sim
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-core --release --features sim --test engine_sim
```

See [TESTING.md](TESTING.md) before adding or changing coverage.
Each crate has 1 integration binary in `tests/it/main.rs`.
Behavior modules share fixtures through `tests/it/support.rs`.
The `sim` and `engine_sim` targets keep their CI names.

Run a behavior module with `cargo test -p zerodb-tape --test it recovery::`.
Debug builds skip slow filesystem tests and seeded tape schedules.
The existing release CI step runs the filesystem checks.
The named simulation commands run 2,000 seeds.
Both sims vary disk headroom around the free-space floor and release space before retrying.
Each seed chooses the capacity phase's generation, covering fresh and recovered writers.
Floor rejections must issue no segment writes, including unpublished frames.
They inject partial-write and sync `StorageFull` errors alongside crash cuts.
The oracles check rejection state, read-only recovery, and acknowledged durability after reopening.
The engine sim also checks dedup state and verifies rejected-key retries commit exactly once.
Failures print the seed, capacity plan, and the existing repro command.
To run the ignored checks in debug:

```sh
cargo test --workspace -- --ignored
```

## License

MIT OR Apache-2.0.
