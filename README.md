# zerodb

Agent memory database in Rust. It stores turn history per session and keyed facts. A write is acked only after it is synced to disk.
Status: early. Don't run it in production. Built so far: the tape, session engine, disk-full handling, server and facts (S1 to S4).

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

That appends over TCP and reads over the Unix socket. A repeated `idem_key` returns `"deduplicated": true`. Facts live at `/v1/facts/put`, `get`, `list` and `delete`. Each key holds 1 current value, and a later put replaces it. With `"if_lsn": <lsn>`, a put writes only if the current value still has that LSN. A mismatch is 409 `conflict`.

## MCP

`zerodb mcp` gives the model `memory_append`, `memory_recent`, `fact_set`, `fact_get` and `fact_list`. Use absolute paths. It starts the server if none is running.
The model can read and write any session in the tenant. Don't give it to a model you don't trust with the whole tenant.

```json
{"mcpServers": {"zerodb": {"command": "/path/to/zerodb", "args": ["mcp", "--config", "/path/to/zerodb.json", "--tenant", "me", "--data-dir", "/path/to/data"]}}}
```

## Shell verbs

The same 5 tools run from a shell, with `zerodb mcp`'s connection flags. Each prints 1 JSON line. `fact set --delete` deletes.

```sh
export ZERODB_TOKEN=$TOKEN
Z="--url unix://$PWD/data/zerodb.sock --agent planner --session run-1"
zerodb append $Z --content "hi" --role user          # {"committed":true,"deduplicated":false,"lsn":3,"seq":2}
zerodb recent $Z --n 10                              # {"truncated":false,"turns":[...]}
zerodb fact set $Z --key repo/payments/retry-rule --content "Retry 3 times, then fail."
zerodb fact get $Z --key repo/payments/retry-rule    # {"fact":{"agent":"planner","content":...}}
zerodb fact list $Z --prefix repo/ --limit 50        # {"facts":[...],"next":null}
```

Each write picks 1 random `idem_key` and retries with it until `--retry-deadline` (default 30 s). Exit 0 means done. Exit 1 means nothing was written, or a read failed. Exit 2 is a usage error.
Exit 3 means the write may have landed. Running the command again uses a new key, so it can write a duplicate.

## Numbers

Durable appends, `fsync` before every ack, Azure L8s v3, medians of 3. Full tables: [bench/H2H.md](bench/H2H.md).

| Local NVMe, 16 writers, closed loop | zerodb | redis | valkey | dicedb |
|---|---|---|---|---|
| Acks/s | 54,784 | 52,922 | 55,287 | 57,439 |
| p99 µs | 462 | 407 | 383 | 367 |

At 16 or more writers on local NVMe, zerodb ties the Redis family on throughput. Its p99 is the worst of the 4 servers in every such run.
At 64 writers with 5 ms think it has the best p50, 149 µs against 184 to 186. Its p99 is 598 µs against 342 to 560.
At 256 writers with 50 ms think its p99 is 631 µs. Redis and Valkey get 303 and 302. #47 tracks the NVMe p99 gap. With 1 writer, in-process SQLite is fastest: 19,746 acks/s against zerodb's 10,218.
On Premium SSD v2, zerodb leads the Redis family on acks/s, p50 and p99 in every run. At 16 writers it acks 10,083/s against redis 4,047.

## How it works

- 1 append-only tape is both WAL and store. Indexes and checkpoints are derived and rebuild from it (D1, I8).
- A zero-filled spare segment is kept ready for the next roll. On ext4 an `fdatasync` is then usually 1 device round trip (D78).
- Group commit never waits on a timer. The writer syncs whatever is queued when it wakes (D80).
- Frames carry location-seeded CRC32C and footers bind whole transactions. Recovery cuts only an unproven tail (D64, D65, D8). Damage inside the last synced group has no later proof, so it can still be cut (I1).
- Windows cost O(returned). Paged arrays, held to a memory budget, map seqs to LSNs and LSNs to offsets (D2, D66, I5).
- Every write carries an `idem_key`, so a retry in the dedup window can't commit twice (D67, I6). A client that can't tell whether a write landed reports `unknown`, never `not_committed` (D75).
- Below a free-space floor the engine goes read-only. Writes are rejected before anything is written. Reads keep working (I14).

D-ids are in the [decision log](docs/design/0002-decision-log.md). I-ids are invariants in [0003](docs/design/0003-slim-v0.1.md), which defines the current scope.
CI runs both SimDisk crash simulations on 2,000 seeds. Anyone with write access to the data directory can edit history. Nothing detects it.

## Sandboxed agents

The harness is the trusted loop that calls the model and starts sandboxes. By default it holds memory (D79). It reads the window and facts, builds the prompt and appends results.
Code in a microVM or container gets no token and no socket, because a token grants a whole tenant. Direct access ([sandboxes.md](docs/deploy/sandboxes.md)) is for code you trust with that tenant, or code that has its own tenant.

## Keys

Each tenant has 1 key space. Keys are 1 to 1024 bytes and never go in URL paths. Agents keep keys apart with prefixes such as `repo/<area>/`. A prefix matches bytes, so `repo/pay` also matches `repo/payments/`. End it with `/` to match whole path segments.

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
- [zerodb-core](crates/zerodb-core/README.md) and [zerodb-tape](crates/zerodb-tape/README.md): engine, formats, recovery and limits. [Python client](clients/python/README.md), [TESTING.md](TESTING.md), [macOS engine runs](bench/RESULTS.md), [0001 (historical)](docs/design/0001-architecture.md).

License: MIT OR Apache-2.0.
