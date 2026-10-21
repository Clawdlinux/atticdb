# S3: server and MCP. Implementation plan

> For agents: one implementer on branch `feat/s3-server-mcp`, TDD, one commit per task. Then two independent reviews (Claude Opus 5.5 and Gemini 4 Argon) before merge.

**Spec:** `docs/design/0003-slim-v0.1.md`, section "S3", invariants I6, I7, I11 and I14, and the non-goals table. Decision D75 in `0002-decision-log.md` (added by T7). The amendments at the end of this plan land in 0003 with T7.

**Goal:** several agent processes share one data directory through one server. They lose nothing when they race to start it, when they retry, or when the server is killed. Agents inside sandboxes and microVMs reach the same server through a Unix socket.

**Owner decisions (2026-10-21):**

1. `zerodb serve` also listens on a Unix domain socket. The path comes from a flag or the config. The socket file is mode 0600 and its parent directory is checked. Auth is the same per-tenant token in the `Authorization` header. Reason: agents run in sandboxes and microVMs (Firecracker, gVisor, Kata, containers). A guest cannot reach the host loopback. Firecracker's vsock device forwards guest `AF_VSOCK` connections to a host Unix socket. Sidecar containers can share a socket through a volume. One Unix listener covers all of them without per-vendor code. The `Host` loopback check does not apply on the Unix socket, because `Host` means nothing there. TCP stays on 127.0.0.1 by default. Non-loopback TCP still needs an explicit flag plus TLS.
2. New page `docs/deploy/sandboxes.md`. Run zerodb on the host or in a sidecar, never inside a disposable sandbox. Connect through Firecracker vsock to the Unix socket, or through a shared socket volume for containers, gVisor and Kata. Durability is only as good as the storage under zerodb. Firecracker's default block `cache_type` is `Unsafe` and does not advertise flush to the guest, so zerodb inside such a guest can lose acknowledged writes on a host crash. Use `Writeback` if it must run inside. gVisor's default root filesystem overlay keeps writes out of the host filesystem, so they do not outlive the sandbox. No container image in S3.
3. Record the decision as D75 in `0002-decision-log.md`, same table format.
4. Out of scope, recorded as non-goals with unblock evidence: BM25 and vector search stay deferred as 0003 says. Hierarchical knowledge-base semantics are covered by S4 key paths (`fact_list(prefix)`). No board or workflow semantics.

**Rules:**

- One new third-party crate: `serde_json`, exact pin. See "Dependencies". No async runtime. No TLS crate in S3 (see open question Q1).
- No `unsafe`. Every crate keeps `#![forbid(unsafe_code)]`.
- The Python client uses the standard library only. `pytest` is a test-only tool, pinned in CI.
- Plain short sentences in docs. No em dashes.
- Commits: `git commit -s` with trailer `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`. Do not push. Do not merge.

**Commands:** the six commands from the S2 plan, plus:

- `cargo test -p zerodb --release --test it -- --ignored` (process-level durability scenarios, opt-in).
- `cargo build -p zerodb --release && ZERODB_BIN=target/release/zerodb python3 -m pytest clients/python/tests`.

## Dependencies

| Need | Choice | Why, in the grounded-design form |
|---|---|---|
| HTTP server and client | Hand-written HTTP/1.1 subset on `std::net` and `std::os::unix::net`, about 400 lines | I am choosing a std-only subset over `hyper` or `tiny_http` because both ends are ours (the MCP adapter, the Python client, plus `curl`), the engine API is blocking so a thread per connection fits, and one small module gives the same server and client code on TCP and Unix sockets with our own bind and permission logic. `hyper` 1.x needs `tokio` and `hyper-util`. `tiny_http` 0.12.0 is from 2022, pulls `ascii`, `chunked_transfer`, `httpdate` and `log`, would still need a separate HTTP client for `zerodb mcp`, and its optional TLS is `rustls` 0.20. |
| JSON | `serde_json = { version = "=1.0.154", default-features = false, features = ["std"] }`, `Value` only | I am choosing `serde_json` over a hand-written JSON codec because MCP and the HTTP bodies need full JSON (string escapes, surrogate pairs, nesting, numbers) and a parser bug there corrupts content. No `serde` derive, so `syn`, `quote` and `proc-macro2` stay out. Its normal dependencies are `serde`, `serde_core`, `itoa`, `memchr` and `zmij`. The implementer re-checks the latest patch at T2 and records the final pin in D75. |
| Constant-time token compare | `blake3` (already pinned `=1.8.7` in zerodb-core) | `blake3::Hash` implements `PartialEq` in constant time (blake3 `src/lib.rs`, doc on `Hash`). Hash the presented token and compare against the hash of every configured token, without early exit. No new crate. |
| Random tokens and idem keys | Read 16 or 32 bytes from `/dev/urandom` | Linux and macOS only, which is the supported set. No `getrandom` crate. |
| Config format | JSON, parsed with `serde_json` | No TOML crate. |
| TLS | None in S3 | Non-loopback TCP is refused at startup until TLS lands. See Q1. |

## T1: engine receipts and byte-budget windows

The server needs two facts the engine does not expose yet.

- Add `deduplicated: bool` to `Appended`. Set it when a retry key returns an earlier receipt, on both paths:
  - the caller fast path in `Engine::append` that resolves a published dedup entry;
  - the writer path where `Machine::prepare` returns `Prepare::Reply`. Mark the staged reply (for example one trailing flag byte after the 8-byte seq) and let `receipt` parse both forms. Fresh commits stay `false`.
- Add a byte budget to windows without breaking I5. New method `Engine::window_budget(scope, query, max_bytes) -> Result<Window>` where `Window { turns: Vec<Turn>, truncated: bool }`. `window` stays and calls it with no budget.
  - `Last(n)` reads newest first and stops before the first turn whose `content.len() + meta.len()` would push the total over `max_bytes`. It returns the kept turns in seq order.
  - `Since { seq, limit }` reads forward and stops the same way.
  - Turns are never cut. A newest turn larger than the budget gives zero turns and `truncated: true`.
  - Tape reads are at most returned turns plus one. Check it with the `window_reads` metric.
- Tests (extend, do not add, per TESTING.md):
  - `engine::` retry scenario: assert `deduplicated` is false on the first commit, true on a fast-path retry, true on a retry that races into the same writer group, and true on a retry after reopening the directory. The last row is the restart idempotency case the server depends on.
  - The existing read-count scenario gains table rows for `max_bytes`: fits all, fits some, newest alone too large, `Since` with a budget. Each row asserts turns, `truncated` and `window_reads`.
- Done when both sims still pass and the README "Session engine" section names the two additions.

## T2: crate skeleton, config, auth and the HTTP subset

- New crate `crates/zerodb`, package `zerodb`, binary `zerodb`. Library modules: `config`, `auth`, `http`, `api`, `listen`, `serve`, `mcp`, `launch`. `main.rs` only parses arguments and calls the library. Depends on `zerodb-core`, `zerodb-tape`, `blake3`, `serde_json`.
- `config`:
  - JSON file: `{"data_dir": "...", "listen": "127.0.0.1:<port>", "unix_socket": "...", "tenants": {"<name>": "<token>"}}`. Flags override fields.
  - The file must be a regular file owned by the current user with mode 0600. Anything looser fails startup and names the path and the mode, never the content.
  - Tenant names follow the engine limit (1 to 255 UTF-8 bytes). Tokens are at least 32 bytes. Duplicate tokens fail.
  - `Debug` for the config redacts tokens.
  - `zerodb init --config <path> --tenant <name>` writes a new 0600 file (create-new, never overwrite) with one random 32-byte hex token, and prints the token once to stdout.
- `auth`: `Authorization: Bearer <token>`. Hash the token with BLAKE3 and compare against every tenant's hash with `blake3::Hash` equality, visiting all tenants. The tenant comes only from the match. A missing or bad token returns 401 with the same body either way.
- `http`: one module for server and client.
  - Request line, headers, `Content-Length` body. HTTP/1.1 keep-alive. HTTP/1.0 closes.
  - Limits: 16 KiB of headers, 64 header lines, body cap 8 MiB (the 4 MiB tape batch plus JSON escaping). Over the cap: 413 and close, without reading the body.
  - Rejected: `Transfer-Encoding` (501), duplicate `Content-Length` or `Host` (400), obs-fold (400), a body on `GET` (400).
  - `Expect: 100-continue` gets `100 Continue` before the body is read. `curl` sends it for large bodies.
  - Timeouts: 10 s to finish request headers, 30 s idle between requests, 30 s per write.
  - The client half writes one request and reads one `Content-Length` response, over `TcpStream` or `UnixStream`.
- Tests: inline `#[cfg(test)]` table tests for the request parser, because it is private logic that is hard to reach from outside (TESTING.md rule 4): one table covering each accept and reject row above. One proptest that random bytes never panic the parser and never yield a body past the cap. Config and auth are tested through the server in T3, not here.
- Done when the parser table and proptest pass and clippy is clean.

## T3: `zerodb serve`, listeners and the HTTP API

- Startup order. This order is what makes the lock race and stale sockets safe:
  1. Load and check the config.
  2. Bind TCP. The default is `127.0.0.1` on a fixed port chosen in this task and documented in the README. Port 0 is allowed for tests. A non-loopback address fails with "non-loopback listening needs TLS, which this build does not have" (Q1).
  3. Open the engine with `TapeOptions::server_address` set to the bound TCP address and the Unix socket path. The tape writes them into `LOCK`. On `DataDirLocked` print the holder line and exit with code 75. Recovery happens here, so this step can take a while.
  4. Unix socket, only now that the lock is held. Default path `<data_dir>/zerodb.sock`.
     - Check the path length against `sun_path` (104 bytes on macOS, 108 on Linux) and fail with a hint to set `--unix-socket`.
     - Check the parent: it exists, is a directory, is owned by the current user, and is not writable by group or others.
     - If the path exists and is a socket, connect to it. A successful connect means another server owns it: fail. A refused connect means a stale socket from a crash: remove it. If the path exists and is not a socket: fail.
     - Bind to `<path>.<pid>.tmp`, set mode 0600, compare the new file's uid with the parent's uid, then rename onto `<path>`. No other user can connect between bind and chmod.
  5. Start the accept threads. Print the TCP address and socket path to stderr.
- Connections: one thread per connection, capped (default 256). Over the cap: 503 `busy` and close. The engine is shared as `Arc<Engine>`. Concurrent appends from many connection threads land in the same writer groups.
- `Host` rule: on TCP the `Host` header must be `127.0.0.1`, `localhost` or `[::1]`, with an optional port. Missing or anything else gets 403. On the Unix socket `Host` is ignored, present or not.
- Routes:
  - `POST /v1/append`. Body `{"agent", "session", "idem_key", "turns": [{"role", "content", "meta"?}]}`. `role` is `system`, `user`, `assistant` or `tool`. `content` is a UTF-8 string. `meta` is a JSON object, stored as its compact text (64 KiB limit). `idem_key` is required, 1 to 128 bytes.
  - `GET /v1/window?agent=&session=&last=N&max_bytes=B` or `...&since_seq=S&limit=N`. `last` and `limit` are capped at 1,000. `max_bytes` defaults to 1 MiB and is capped at 8 MiB. Response `{"turns": [{"seq", "lsn", "commit_time_ns", "role", "content", "meta"}], "truncated": bool}`. Content that is not UTF-8 (only possible from direct Rust API writes) comes back with U+FFFD and `"lossy": true` on that turn.
  - `GET /healthz`. No token. 200 `{"status": "ok"}`, 200 `{"status": "read_only"}`, 503 `{"status": "poisoned"}`. Read-only stays 200 on purpose, as 0003 says.
- Write responses always carry `outcome`. Clients branch on `outcome`, never on the status code alone.

  | Engine result | Status | Body |
  |---|---|---|
  | `Appended` | 200 | `{"outcome": "committed", "deduplicated", "txn", "first_lsn", "last_lsn", "seqs"}` |
  | `InvalidInput`, bad JSON, missing `idem_key` | 400 | `{"outcome": "not_committed", "error": "invalid_input", "message"}` |
  | No or bad token | 401 | `{"outcome": "not_committed", "error": "unauthorized"}` |
  | Bad `Host` on TCP | 403 | `{"outcome": "not_committed", "error": "forbidden_host"}` |
  | `IdempotencyConflict` | 409 | `{"outcome": "not_committed", "error": "idempotency_conflict"}` |
  | Body over cap | 413 | `{"outcome": "not_committed", "error": "too_large"}` |
  | `ResourceExhausted(r)` | 429 | `{"outcome": "not_committed", "error": "resource_exhausted", "resource": r}` |
  | `Storage(DiskFull)` | 507 | `{"outcome": "not_committed", "error": "disk_full"}` |
  | `Poisoned` | 503 | `{"outcome": <engine outcome>, "error": "poisoned"}` |
  | Any other storage error | 500 | `{"outcome": <engine outcome>, "error": "storage"}` |

- Logging: one stderr line per request with method, path, status, tenant name and latency. Never the `Authorization` header, never a body, never a query string (it holds session names). Errors from config parsing never echo file content.
- Also wire `zerodb recover --truncate-at <lsn> --data-dir <dir>` to `Tape::recover_truncate_at`. The README promised it with the server binary.
- `serve` is a library function `serve::run(engine: Arc<Engine>, listeners, config)`, so tests can pass an engine built on `SimDisk`.
- Tests in `crates/zerodb/tests/it/` (`main.rs`, `support.rs`, `api.rs`, `listen.rs`). One scenario per path, many assertions:
  - `api::append_window_and_retry_over_both_listeners`: for each listener (TCP, Unix), append, retry the same key (`deduplicated: true`, same txn and seqs), retry with a changed payload (409), then window with a `max_bytes` table. Drop the server, reopen the engine on the same directory, retry the first key: `deduplicated: true` with the original receipt.
  - `api::auth_and_host_rules`: one table. Rows: no token, bad token, token of tenant B reading tenant A's session (empty window, never A's data, I11), good token. Host rows: loopback names pass on TCP, `evil.example` gets 403 on TCP, the same `evil.example` and a missing `Host` pass on Unix. This Unix row is the Firecracker guest case.
  - `api::health_tracks_engine_state`: engine on `SimDisk`. Ok gives 200 ok. A capacity limit under the floor makes an append return 507 `not_committed` and health 200 `read_only`, while windows still serve. Freeing space and retrying the same key commits once. An injected sync failure gives 503 `poisoned` and an `unknown` or `not_committed` outcome as the engine reports it.
  - `listen::socket_file_rules`: socket mode is 0600; a stale socket from a dropped server is replaced; a live socket owned by another server fails startup; a regular file at the path fails; a group-writable parent fails; a path past `sun_path` fails with the hint. A second `serve` on the same data dir exits with code 75 and leaves the first server's socket working.
- Done when all four tests pass on Linux and macOS.

## T4: `zerodb mcp`

- Stdio adapter. Newline-delimited JSON-RPC 2.0 on stdin and stdout. Logs to stderr only.
- MCP version. I am choosing the legacy `initialize` handshake (revision `2025-11-25`) over dual-era support because the 2026-07-28 stdio binding tells dual-era clients to fall back to `initialize` on any error that is not a recognized modern error ([stdio binding](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio), "Backward Compatibility"). That covers today's clients and dual-era ones. Pre-`initialize` requests, including `server/discover`, get `-32601`. On `initialize`, echo the client's version if it is `2025-11-25`, `2025-06-18` or `2025-03-26`, else answer `2025-11-25`. Modern-only clients are Q3.
- Methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`. `notifications/cancelled` is ignored: a cancelled write may still commit, and that is safe because it carries a key.
- Tools. The model never sees keys, tokens or tenants.
  - `memory_append {content, role? = "assistant", session?, meta?}`. Returns text `committed seq=<n>` and `structuredContent {seq, lsn, deduplicated}`.
  - `memory_recent {n? = 20 (max 200), session?, max_bytes? = 64 KiB}`. Returns the turns as text and as `structuredContent`.
  - `agent` comes from `--agent` (default `mcp`). `session` defaults to `--session` (default `default`). A tool may pass `session`. That stays inside the token's tenant, so I11 holds. Q6 asks the owner to confirm.
  - Errors return `isError: true` with the outcome in the text, so the model knows whether anything was written.
- Connecting (`launch` module). Inputs: `--url unix:///path` or `--url http://127.0.0.1:<port>`, `--data-dir`, `--config`, token from `ZERODB_TOKEN`, `--token-file` (0600) or the config entry for `--tenant`.
  1. If `--url` is set, use it. Done.
  2. Else read `<data_dir>/LOCK`. If it names an address, prefer the socket, and probe `GET /healthz`. Any 200 or 503 answer means a server is there. Use it.
  3. Else spawn `zerodb serve --config <path>` with stdin null, stdout and stderr appended to `<data_dir>/serve.log`, and `process_group(0)` so it outlives the adapter. Never pass the token on the command line.
  4. Poll step 2 until a server answers or `--start-timeout` (default 60 s) passes. A spawned server that exits with code 75 lost the lock race. That is expected: keep polling, the winner is recovering or binding.
- One `idem_key` per tool call: 16 random bytes, hex. Every retry of that call reuses it.
- Retry rule, shared with the Python client: retry on a transport error (connect, reset, timeout, short or malformed response) and on `outcome: unknown`. Nothing else is retried. Backoff 50 ms doubling to 2 s with jitter, until `--retry-deadline` (default 30 s). On a transport error, rerun the connect steps first. A killed server is then restarted by whichever adapter notices first. `not_committed` is returned to the model at once: nothing was written, so a new call cannot duplicate.
- When the deadline passes on `unknown`, return `isError` with `outcome: unknown`. Say plainly in the text that the write may have been saved.
- Tests in `tests/it/mcp.rs`:
  - `mcp::tool_calls_over_stdio`: spawn `zerodb mcp` against a running test server. Drive `initialize`, `tools/list`, two `memory_append` calls and `memory_recent`. Assert the protocol version echo, tool schemas, `-32601` for a pre-initialize `server/discover`, seqs, and that no `idem_key` appears in any message the model sees.
  - `mcp::launch_race_starts_one_server`: start two adapters at once on an empty data dir with no server. Exactly one `serve` holds the lock, the other exits 75, and both adapters' first `memory_append` commits.
- Done when both pass.

## T5: Python client

- `clients/python/zerodb_client.py`, one file, standard library only (`http.client`, `socket`, `json`, `secrets`, `time`, `random`).
- `Client(url, token)`. URL forms: `http://127.0.0.1:<port>`, `unix:///path`, and `vsock://<cid>:<port>` on Linux through `socket.AF_VSOCK`. The vsock form is the guest side of the Firecracker path in owner decision 1, about 10 lines (Q5).
- `append(agent, session, turns, idem_key=None)` creates one key per call when none is given and applies the retry rule from T4 with the same key. Returns the decoded receipt including `deduplicated`. Raises `NotCommitted` or `OutcomeUnknown` with the server's error.
- `window(agent, session, last=None, since_seq=None, limit=None, max_bytes=None)` and `health()`.
- Never puts the token in an exception message or `repr`.
- Tests (`clients/python/tests/conftest.py`, `helpers.py`, `test_client.py`): a fixture starts `$ZERODB_BIN serve` on port 0 and a temp socket, inside a pytest `tmp_path`.
  - `test_append_retry_and_window` parametrized over `http` and `unix`: append, retry with the same key, window with `max_bytes`.
  - `test_retry_survives_server_restart`: append with a fixed key, kill the server with SIGKILL before reading the reply (helper closes the socket after sending), restart, call again with the same key. Exactly one copy in the window.
  - vsock is not tested in CI. It is covered by the docs page.
- Done when pytest passes against the release binary on Linux and macOS.

## T6: the done criteria, end to end

- `tests/it/durability.rs`, `#[ignore = "spawns processes and kills the server"]`, run in its own CI step.
- `durability::two_mcp_clients_and_a_killed_server_lose_nothing`:
  - Client H is a host agent: `zerodb mcp --data-dir D --config C`. It can start the server.
  - Client G simulates a Firecracker guest: `zerodb mcp --url unix://<D>/zerodb.sock`. It never sees `D` and cannot start a server. Its requests carry a non-loopback `Host`, as a guest's would.
  - Both start before any server exists, so H also exercises the launch path.
  - Each sends 500 `memory_append` calls with unique content, from 2 threads each. Midway the test kills the server with SIGKILL, twice, at random moments from a seeded RNG printed on failure.
  - H restarts the server on its next transport error. G retries over the socket until it is back.
  - Oracle, read back over HTTP: every call that returned `committed` appears exactly once. Calls that returned `unknown` appear at most once. Seqs per session are dense. At least one retry returned `deduplicated: true` (asserts the kill actually landed mid-write; rerun with a new seed if not).
- `durability::killed_server_restarts_with_every_acknowledged_write`: a plain Rust HTTP client writes over HTTP from 8 threads, kill SIGKILL, restart, compare against the acknowledged set. Same oracle. This is the 0003 done criterion stated without MCP in the way.
- CI: add the `--ignored` step above and the pytest step from T5 to `.github/workflows/ci.yml`, on both OS rows. Pin `pytest` by exact version in the step.
- Done when both scenarios pass 20 runs in a row locally on macOS and once in CI on both OS rows.

## T7: docs and decision

- `docs/deploy/sandboxes.md`, per owner decision 2:
  - Where to run: on the host or in a sidecar. Never inside a disposable sandbox.
  - Firecracker: set the vsock `uds_path` to `<dir>/v.sock` and `zerodb serve --unix-socket <dir>/v.sock_<port>`. Firecracker forwards a guest connection to port `P` on CID 2 to `<uds_path>_P` ([vsock.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md), "Guest-Initiated Connections"). Ports from 2^30 up are reserved. In the guest, use the Python client's `vsock://2:<port>` or bridge with `socat UNIX-LISTEN:/run/zerodb.sock,fork VSOCK-CONNECT:2:<port>` and point `zerodb mcp --url` at it.
  - Containers, gVisor, Kata: mount the socket's directory into the sandbox and use `unix://`.
  - The socket is 0600, so the connecting process must run as the same uid as `zerodb serve`. This includes the Firecracker or jailer process (Q2).
  - Durability warning. Firecracker's block `cache_type` defaults to `Unsafe`, which does not advertise flush, so zerodb inside such a guest can lose acknowledged writes on a host crash. Use `Writeback` if it must run inside ([block-caching.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/api_requests/block-caching.md)). gVisor's default `--overlay2=root:self` keeps root filesystem writes in an overlay, not the host filesystem ([gVisor filesystem guide](https://gvisor.dev/docs/user_guide/filesystem/)). They do not outlive the sandbox.
  - No container image yet.
- Root `README.md`: "What exists" gains the server, MCP and Python client. A quick start: `zerodb init`, `zerodb serve`, one `curl` append and window over TCP and over `--unix-socket`, and an MCP client config snippet. Remove "CLI wiring arrives with the server binary in S3".
- `crates/zerodb/README.md`: the API table from T3, the retry rule, the connect steps, exit codes, and that an auto-started server keeps running after its adapters exit.
- `crates/zerodb-core/README.md`: `deduplicated` and `window_budget`.
- `0002-decision-log.md`: add D75 (text below).
- `0003`: apply the amendments below.

## Amendments to 0003

Applied in T7, exact text.

**A1. Non-goals table.** Add three rows after "Markdown import". The BM25 and vector rows stay as they are.

```markdown
| Hierarchical knowledge base, boards, workflows | S4 key paths with `fact_list(prefix)` cannot express a structure that dogfood needs |
| Container image | A user runs zerodb as a sidecar and asks for a published image |
| TLS and non-loopback TCP | A user needs to reach zerodb from another machine and a TLS proxy on loopback does not serve them |
```

The TLS row lands only if the owner answers Q1 with "refuse in S3".

**A2. Invariants table.** Add I15 after I14. Update the "Checked by" cells of I6, I7 and I11 to add the S3 tests.

```markdown
| I15 | Only a token picks a tenant. TCP serves loopback `Host` names only and refuses non-loopback binds without TLS. The Unix socket is mode 0600, bound under a parent the owner alone can write, and only the lock holder may replace its path | Bind order: TCP, lock, then socket. Temp bind, chmod, rename. Constant-time BLAKE3 compare over every token | `zerodb --test it`: `api::auth_and_host_rules`, `listen::socket_file_rules` |
```

- I6 add: `zerodb --test it`: `api::append_window_and_retry_over_both_listeners`, `durability::two_mcp_clients_and_a_killed_server_lose_nothing`.
- I7 add: `mcp::launch_race_starts_one_server`, `listen::socket_file_rules`.
- I11 add: `api::auth_and_host_rules`.

**A3. S3 section.** Replace the first two bullets with:

```markdown
- `zerodb serve` holds the lock. HTTP/JSON on TCP bound to 127.0.0.1 by default. Other addresses need an explicit flag and TLS. S3 has no TLS, so it refuses them.
- It also listens on a Unix domain socket, default `<data_dir>/zerodb.sock`, set by flag or config. The socket file is mode 0600. Its parent must be owned by the server's user and not writable by group or others. Only the lock holder may remove a stale socket. Agents in sandboxes and microVMs reach the host through it: Firecracker forwards guest vsock connections to a host Unix socket, and containers, gVisor and Kata can share it through a volume. See `docs/deploy/sandboxes.md`.
- Auth: one token per tenant in a config file that must be mode 0600. Token only in the `Authorization` header, compared in constant time. The tenant comes from the token, never from the request. On TCP the `Host` header must be a loopback name. On the Unix socket `Host` has no meaning and is not checked.
```

Replace "whether it was deduplicated" with "a `deduplicated` flag". Add to the `GET /v1/window` bullet: "Turns are never cut. The response says when older turns were left out." Add to the done bullet: "An MCP client in a simulated Firecracker guest, which only reaches the Unix socket, loses nothing either."

**A4. Decisions table at the end of 0003.** Add:

```markdown
| Unix socket in S3? | Yes. It is how sandboxed and microVM agents reach the host (owner, 2026-10-21, D75) |
| Run zerodb inside a sandbox? | No. Host or sidecar. A guest's storage may drop flushes (Firecracker `Unsafe`) or vanish (gVisor overlay) |
```

## D75 text

Under "Slim v0.1 (0003)", after D74:

```markdown
| D75 | S3 serves HTTP on loopback TCP and on a 0600 Unix socket, with the same token auth. The `Host` check applies to TCP only. zerodb runs on the host or in a sidecar, never in a disposable sandbox. No container image in S3. New crate: `serde_json =1.0.154`, defaults off, `std`, `Value` only. HTTP is a std-only HTTP/1.1 subset | Guests cannot reach host loopback. Firecracker vsock and shared volumes both end at a host Unix socket, so one listener covers them. Guest storage can drop flushes or vanish. `hyper` needs an async runtime and `tiny_http` is unmaintained and still needs a client |
```

## Risks

| Risk | What could go wrong | Mitigation in this plan |
|---|---|---|
| Lock race between `zerodb mcp` launchers | Two adapters each start a server. Both write, or one unlinks the other's socket | Only the engine lock decides. TCP binds before the lock, the socket only after it. A loser exits 75 and never touches the socket. Adapters poll `LOCK`. Tested in `mcp::launch_race_starts_one_server` and T6 |
| Winner still recovering | Open time is O(log size) (I13). Pollers give up before the server answers | `--start-timeout` (60 s default). The timeout error names `serve.log`. Docs state open time grows with the log |
| Stale socket after a crash | `kill -9` leaves the socket file. A restart cannot bind | The lock holder probes it. Refused means stale: remove and rebind. A live peer or a non-socket file fails startup |
| Socket permission window | Someone connects between `bind` and `chmod` | Bind a temp name, chmod 0600, then rename onto the final path. Parent ownership check |
| Token leakage | Tokens show up in logs, panics, `ps`, or `Debug` output | Never log headers, bodies or queries. Redacted `Debug`. Tokens never on a command line. Config errors never echo content. Python client hides the token from `repr` and exceptions |
| Idempotency across restart | A retry after a restart writes twice | The `idem_key` is in the turn payload. Recovery and checkpoints rebuild the dedup table (I6, I8). Tested by the reopen row in T1 and T3 and by T6. A retry after the key leaves the bounded window writes again. Documented |
| Retries exhausted | The model calls again after an `unknown`, with a new key, and duplicates a turn | The tool error says the write may be saved. The 30 s deadline is far below the dedup window. Residual risk is documented |
| Poisoned server stays up | A sync failure poisons the engine. It keeps serving reads (D38) and never restarts itself, so auto-start never replaces it | Health is 503 `poisoned`. Docs tell operators to restart. Q4 asks whether to add `--exit-on-poison` |
| uid mismatch with a jailer | Firecracker's jailer drops to another uid and cannot connect to a 0600 socket | Documented. Q2 asks whether to allow a group mode |
| `sun_path` length | Deep data directories exceed 104 or 108 bytes | Startup fails with a hint to set `--unix-socket` |
| Slow or idle clients | Connections pile up and starve threads | Header, idle and write timeouts. Connection cap with 503 `busy` |
| HTTP subset gaps | A real client sends something the subset rejects | Rejections are explicit status codes. `Expect: 100-continue` handled. Python and Rust clients tested. A `curl` example in the README is run by hand at review |
| Orphan servers | An auto-started server lives on after every adapter exits | Documented, with how to stop it. It holds no unsynced state, so `kill` is always safe |

## Open questions for the owner

- **Q1. TLS.** S3 refuses non-loopback TCP because it ships no TLS. Adding TLS now means `rustls` plus a crypto provider and a PEM loader. OK to defer, with the A1 non-goal row?
- **Q2. Socket mode.** 0600 forces the Firecracker jailer or a sidecar to share zerodb's uid. Keep 0600 only, or allow an opt-in `--unix-socket-group` with 0660?
- **Q3. MCP version.** Legacy `initialize` (2025-11-25) only. Modern-only 2026-07-28 clients will fail. Defer until such a client is in use?
- **Q4. Poison.** Keep serving reads when poisoned (D38), or add `--exit-on-poison` so `zerodb mcp` can restart it?
- **Q5. Guest client.** Python gets `vsock://` through stdlib. `zerodb mcp` in a guest needs a `socat` bridge, because std Rust has no `AF_VSOCK`. Acceptable for S3?
- **Q6. MCP session argument.** Tools may pass `session` within the token's tenant. Or should session come only from adapter flags, as 0001 had it?
