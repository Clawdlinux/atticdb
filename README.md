# zerodb

Agent memory database in Rust. A write is acknowledged only after it is synced to disk.
Status: early, not for production. Steps S1 to S4 exist: the tape, session engine, disk-full handling, server and facts.

## Quick start

```sh
cargo build -p zerodb --release
export PATH="$PWD/target/release:$PATH"
TOKEN=$(zerodb init --config zerodb.json --tenant me)   # 0600 config, token printed once
zerodb serve --config zerodb.json --data-dir ./data &    # TCP 127.0.0.1:7425 and ./data/zerodb.sock
curl -s -H "Authorization: Bearer $TOKEN" \
  -d '{"agent":"planner","session":"run-1","idem_key":"k1","turns":[{"role":"user","content":"hi"}]}' \
  http://127.0.0.1:7425/v1/append
curl -s --unix-socket data/zerodb.sock -H "Authorization: Bearer $TOKEN" \
  'http://localhost/v1/window?agent=planner&session=run-1&last=10'
```

That appends over TCP and reads over the Unix socket. A repeated `idem_key` returns `"deduplicated": true`.
Facts live at `/v1/facts/put`, `get`, `list` and `delete`. Each key holds 1 value, and a later put replaces it. With `"if_lsn": <lsn>`, a put writes only if the current value still has that LSN. A mismatch is 409 `conflict`.

## MCP

`zerodb mcp` gives the model `memory_append`, `memory_recent`, `fact_set`, `fact_get` and `fact_list`. Use absolute paths. It starts the server if none is running.

```json
{"mcpServers": {"zerodb": {"command": "/path/to/zerodb", "args": ["mcp", "--config", "/path/to/zerodb.json", "--tenant", "me", "--data-dir", "/path/to/data"]}}}
```

## Shell verbs

The same 5 tools run from a shell. Each prints 1 JSON line, and `fact set --delete` deletes.

```sh
export ZERODB_TOKEN=$TOKEN
Z="--url unix://$PWD/data/zerodb.sock --agent planner --session run-1"
zerodb append $Z --content "hi" --role user          # {"committed":true,"deduplicated":false,"lsn":4,"seq":2}
zerodb recent $Z --n 10                              # {"truncated":false,"turns":[...]}
zerodb fact set $Z --key repo/payments/retry-rule --content "Retry 3 times, then fail."
zerodb fact get $Z --key repo/payments/retry-rule    # {"fact":{"agent":"planner","content":...}}
zerodb fact list $Z --prefix repo/ --limit 50        # {"facts":[...],"next":null}
```

A write retries with 1 key until `--retry-deadline` (default 30 s). Exit 0 is done, 1 means nothing was written or a read failed, and 2 is a usage error.
Exit 3 means the write may be saved, and repeating it may duplicate it.

## Numbers

Durable appends, `fsync` before every ack, Azure L8s v3, medians of 3. Full tables: [bench/H2H.md](bench/H2H.md).

| Local NVMe, 16 writers, closed loop | zerodb | redis | valkey | dicedb |
|---|---|---|---|---|
| Acks/s | 54,784 | 52,922 | 55,287 | 57,439 |
| p99 µs | 462 | 407 | 383 | 367 |

NVMe, 64 writers, 5 ms think: zerodb p50, p99 and p99.9 are 149, 598 and 959 µs. Redis gets 186, 560 and 1,130 µs.
With 1 writer on NVMe, in-process SQLite is fastest: 19,746 acks/s against zerodb's 10,218. zerodb loses on p99 at 256 writers with 50 ms think: 631 µs against redis 303.
On Premium SSD v2 at 16 writers, zerodb acks 10,083/s against redis 4,047.

## How it works

- One append-only tape is both WAL and store. Indexes and checkpoints are derived and rebuild from it (D1).
- Spare segments are zero-filled in the background. On ext4 each `fdatasync` is then 1 device round trip (D78).
- Group commit never waits on a timer. The writer syncs whatever is queued when it wakes (D80).
- Frames carry location-seeded CRC32C and footers bind whole transactions. Recovery cuts only an unproven tail (D64, D65, D8, I1).
- Windows cost O(returned). Paged arrays, held to a memory budget, map seqs to LSNs and LSNs to offsets (D2, D66, I5).
- Every write has an `idem_key`. Clients never downgrade `unknown`. The dedup map has 64 shards (D67, D81).
- Below a free-space floor the engine goes read-only. Writes are rejected before anything is written. Reads keep working (I14).

D-ids: [decision log](docs/design/0002-decision-log.md). I-ids: invariants in [0003](docs/design/0003-slim-v0.1.md), the current scope.
CI runs both SimDisk crash simulations on 2,000 seeds. There is no tamper evidence: write access can edit history.

## Sandboxed agents

By default the harness holds memory. It reads the window and facts, builds the prompt and appends results (D79).
Code in a microVM or container gets no token and no socket, because a token grants a whole tenant. Direct access ([sandboxes.md](docs/deploy/sandboxes.md)) is for code you trust with that tenant, or with its own.

## Keys

Each tenant has 1 key space. Keys are 1 to 1024 bytes and never go in URL paths.
Agents keep keys apart with prefixes such as `repo/<area>/`. A prefix matches bytes, so end it with `/` for path semantics.

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
cargo test --workspace && cargo test --workspace --release
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-tape --release --features sim --test sim
ZERODB_SIM_SEEDS=2000 cargo test -p zerodb-core --release --features sim --test engine_sim
```

## Docs

- [zerodb](crates/zerodb/README.md): server, HTTP API, MCP, `stats`, `load`, `verify`, `dump` and `recover`.
- [zerodb-core](crates/zerodb-core/README.md) and [zerodb-tape](crates/zerodb-tape/README.md): engine, formats, recovery and limits. [Python client](clients/python/README.md), [TESTING.md](TESTING.md), [G1 on macOS](bench/RESULTS.md), [0001 (historical)](docs/design/0001-architecture.md).

License: MIT OR Apache-2.0.
