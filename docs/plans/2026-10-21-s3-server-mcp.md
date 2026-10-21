# S3: server and MCP. Implementation plan

> For agents: one implementer on branch `feat/s3-server-mcp`, TDD, one commit per task. Then two independent reviews (Claude Opus 5.5 and Gemini 4 Argon) before merge.

**Spec:** `docs/design/0003-slim-v0.1.md`, section "S3", invariants I4, I6, I7, I11 and I14, and the non-goals table. Decision D75 in `0002-decision-log.md` (added by T7). The amendments at the end of this plan land in 0003 with T7.

**Status:** plan review round 1 done. Both reviewers approved with changes. This revision applies them and the owner's answers below.

**Goal:** several agent processes share one data directory through one server. They lose nothing when they race to start it, when they retry, or when the server is killed. Agents inside sandboxes and microVMs reach the same server through a Unix socket.

**Owner decisions (2026-10-21):**

1. `zerodb serve` also listens on a Unix domain socket. The path comes from a flag or the config. The socket file is mode 0600 and its parent directory is checked. Auth is the same per-tenant token, sent with the Bearer scheme in the `Authorization` header. Reason: agents run in sandboxes and microVMs (Firecracker, gVisor, Kata, containers). A guest cannot reach the host loopback. Firecracker's vsock device forwards guest `AF_VSOCK` connections to a host Unix socket. Sidecar containers can share a socket through a volume. One Unix listener covers all of them without per-vendor code. The `Host` loopback check does not apply on the Unix socket, because `Host` means nothing there. TCP stays on 127.0.0.1 by default. Non-loopback TCP still needs an explicit flag plus TLS.
2. New page `docs/deploy/sandboxes.md`. Run zerodb on the host or in a sidecar, never inside a disposable sandbox. Connect through Firecracker vsock to the Unix socket, or through a shared socket volume for containers, gVisor and Kata. Durability is only as good as the storage under zerodb. Firecracker's default block `cache_type` is `Unsafe` and does not advertise flush to the guest, so zerodb inside such a guest can lose acknowledged writes on a host crash. Use `Writeback` if it must run inside. gVisor's default root filesystem overlay keeps writes out of the host filesystem, so they do not outlive the sandbox. No container image in S3.
3. Record the decision as D75 in `0002-decision-log.md`, same table format.
4. Out of scope, recorded as non-goals with unblock evidence: BM25 and vector search stay deferred as 0003 says. Hierarchical knowledge-base semantics are covered by S4 key paths (`fact_list(prefix)`). No board or workflow semantics.

**Owner answers to the plan's open questions (2026-10-21):**

| Q | Answer |
|---|---|
| Q1 TLS | S3 refuses non-loopback TCP. TLS is deferred. A TLS proxy on loopback covers remote access |
| Q2 socket mode | Opt-in `--unix-socket-group <gid or name>` makes the socket mode 0660 with that group. Default stays 0600 |
| Q3 MCP version | Legacy `initialize` only. Answer `ping` before `initialize`, which the 2025-11-25 lifecycle allows. Echo `2024-11-05` when a client asks for it |
| Q4 poison | Add `--exit-on-poison`. `zerodb mcp` always passes it when it auto-starts a server |
| Q5 guest client | A `socat` bridge is fine for `zerodb mcp` in a guest. Std Rust has no `AF_VSOCK` |
| Q6 session argument | Tools may pass `session`. The tenant still comes only from the token |

**Rules:**

- Two new third-party crates, both exact pins: `serde_json` and `httparse`. One new feature on the existing `rustix` pin: `process`. See "Dependencies". No async runtime. No TLS crate.
- No `unsafe`. Every crate keeps `#![forbid(unsafe_code)]`.
- The Python client uses the standard library only. `pytest` is a test-only tool, pinned in CI.
- Plain short sentences in docs. No em dashes.
- Commits: `git commit -s` with trailer `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`. Do not push. Do not merge.

**Commands:** the six commands from the S2 plan, plus:

- `cargo test -p zerodb --release --features fault-inject --test it -- --include-ignored` (API, MCP and process-level durability scenarios).
- `cargo build -p zerodb --release --features fault-inject && ZERODB_BIN=target/release/zerodb python3 -m pytest clients/python/tests`.

## Dependencies

| Need | Choice | Why, in the grounded-design form |
|---|---|---|
| HTTP head parsing | `httparse = { version = "=1.10.1", default-features = false }` | I am choosing `httparse` over a hand-written request-line and header parser because it is the parser `hyper` uses, it has no dependencies, and parser bugs are the main risk in a small HTTP server. We still write the framing around it: limits, deadlines, `Content-Length` and keep-alive. |
| HTTP server and client framing | Hand-written HTTP/1.1 subset on `std::net` and `std::os::unix::net`, about 300 lines | I am choosing a std-only subset over `hyper` or `tiny_http` because both ends are ours (the MCP adapter, the Python client, plus `curl`), the engine API is blocking so a thread per connection fits, and one small module gives the same server and client code on TCP and Unix sockets with our own bind and permission logic. `hyper` 1.x needs `tokio` and `hyper-util`. `tiny_http` 0.12.0 is from 2022, pulls `ascii`, `chunked_transfer`, `httpdate` and `log`, would still need a separate HTTP client for `zerodb mcp`, and its optional TLS is `rustls` 0.20. |
| JSON | `serde_json = { version = "=1.0.154", default-features = false, features = ["std"] }`, `Value` only | I am choosing `serde_json` over a hand-written JSON codec because MCP and the HTTP bodies need full JSON (string escapes, surrogate pairs, nesting, numbers) and a parser bug there corrupts content. No `serde` derive, so `syn`, `quote` and `proc-macro2` stay out. Its normal dependencies are `serde`, `serde_core`, `itoa`, `memchr` and `zmij`. |
| Current effective uid | `rustix =1.1.2` (already pinned by D71) gains the `process` feature, for `rustix::process::geteuid` | I am choosing the `process` feature over a std-only trick (create a file, read its owner) because the config, token-file and socket-parent checks need the uid before we create anything, and `forbid(unsafe_code)` rules out raw `libc`. Same pin, one more feature. On Linux it only enables `linux-raw-sys/prctl`, which is already in the tree. |
| Constant-time token compare | `blake3` (already pinned `=1.8.7` in zerodb-core) | `blake3::Hash` implements `PartialEq` in constant time (blake3 `src/lib.rs`, doc on `Hash`). Hash the presented token and compare against the hash of every configured token, without early exit. No new crate. |
| Random tokens, instance ids, idem keys | Read 16 or 32 bytes from `/dev/urandom` | Linux and macOS only, which is the supported set. No `getrandom` crate. |
| Group name to gid | Numeric gid, or a name looked up by parsing `/etc/group` | No NSS lookup without `libc`. Groups from LDAP or other NSS sources need the numeric gid. Documented |
| Config format | JSON, parsed with `serde_json` | No TOML crate. |
| TLS | None | Non-loopback TCP is refused at startup (Q1). |

The implementer re-checks the latest patch of `serde_json` and `httparse` at T2 and records the final pins in D75.

## The outcome rule (applies to T3, T4 and T5)

Every write response carries `outcome`: `committed`, `not_committed` or `unknown`. Clients branch on `outcome`, never on the status code alone. A client never reports less than it knows.

One tool call or `append` call is one `idem_key` and one merged result:

| Previous attempts of this call | This attempt returns | Merged result | Next step |
|---|---|---|---|
| Any | `committed` | `committed` | Return |
| None | `not_committed` | `not_committed` | Return. Nothing was written, so the caller may call again |
| At least one transport error or `unknown` | `not_committed` | stays `unknown` | Retry with the same key until the deadline |
| Any | transport error or `unknown` | `unknown` | Retry with the same key until the deadline |
| Deadline passes | | the merged state | Return `unknown` if any attempt was unclear |

Why a later `not_committed` cannot clear `unknown`: two engine paths return `not_committed` for a key whose first attempt may already be durable. The floor rejection (507) runs before preparation and does not look at the key's fate in a poisoned or recovering writer. The dedup fast path returns the stored poison error before it resolves the key (`Engine::append`, the `state.failure` check, engine.rs:264). Only `committed` replaces `unknown`.

**Committed without a receipt.** `Engine::append` can fail after the write is durable, while it rebuilds the receipt (`receipt`, engine.rs:344-363). It then returns `Poisoned` or `CorruptIndex` with `Outcome::Committed`. The server answers 200 with `{"outcome": "committed", "receipt": null, "error": "<kind>"}`. Clients treat it as committed and never retry it. The Python client returns a receipt with `receipt_missing: true` and null fields. The MCP tool returns success text "committed, receipt unavailable" and `structuredContent {committed: true, seq: null, lsn: null, deduplicated: null}`.

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

## T2: crate skeleton, config, auth and HTTP framing

- New crate `crates/zerodb`, package `zerodb`, binary `zerodb`. Library modules: `config`, `auth`, `http`, `api`, `listen`, `serve`, `mcp`, `launch`, `outcome`. `main.rs` only parses arguments and calls the library. Depends on `zerodb-core`, `zerodb-tape`, `blake3`, `serde_json`, `httparse`, `rustix` (`fs`, `process`, `std`). `fs` is already on the pin through `zerodb-tape` and gives `O_NONBLOCK`.
- Cargo feature `fault-inject`, off by default. It adds a `FaultIo` that wraps `StdIo` and fails the next sync of a segment file in the write path when the file `<data_dir>/FAULT-SYNC-ONCE` exists. Checkpoint, index and directory syncs pass through, so the injected failure always lands on an append. It starts disarmed, because recovery can open and sync segments when it repairs a torn tail. The server calls `FaultIo::arm` after `Engine::open` returns. It removes the trigger file, then returns `SyncFailed`. The data bytes stay written, as with a real sync failure. The release binary never has it.
- `config`:
  - JSON file: `{"data_dir": "...", "listen": "127.0.0.1:<port>" or "none", "unix_socket": "...", "unix_socket_group": "...", "tenants": [{"name": "<name>", "token": "<token>"}]}`. Flags override fields. `tenants` is an array, so a duplicate name fails instead of the last JSON key silently winning.
  - Open the file once. Open with `O_NONBLOCK`, so a FIFO cannot block the open. Check the opened handle with `File::metadata` (an `fstat`, so no race with a rename): a regular file, owned by `geteuid()`, no group or other bits (mode & 0o077 == 0). Read the content from that same handle. A failed check names the path and the mode, never any content.
  - `--token-file` for `zerodb mcp` uses the same open-then-fstat check.
  - Tenant names follow the engine limit (1 to 255 UTF-8 bytes). Tokens are at least 32 bytes. Duplicate tokens fail.
  - `Debug` for the config redacts tokens.
  - `zerodb init --config <path> --tenant <name>` writes a new 0600 file (create-new, never overwrite) with one random 32-byte hex token, and prints the token once to stdout.
- `auth`: the Bearer scheme in the `Authorization` header. Hash the presented token with BLAKE3 and compare against every tenant's hash with `blake3::Hash` equality, visiting all tenants. The tenant comes only from the match. A missing or bad token returns 401 with the same body either way.
- `http`: one module for server and client. `httparse` parses the request line and headers. Our code does the rest.
  - Limits: 16 KiB of headers, 64 header lines, body cap 8 MiB (the 4 MiB tape batch plus JSON escaping).
  - `Content-Length` must be 1 to 19 ASCII digits. No sign, no spaces, no list. Anything else is 400.
  - Rejected: `Transfer-Encoding` (501), duplicate `Content-Length` or `Host` (400), obs-fold (400), a body on `GET` (400).
  - Order per request: read and parse the head, then check `Host` and auth, then the length cap, then read the body. A request that fails `Host`, auth or the cap gets its error and the connection closes without reading the body.
  - Any 4xx from parsing or framing closes the connection after the response. Keep-alive continues only after a well-formed request.
  - `Expect: 100-continue` gets `100 Continue` only after the head passes the checks above. `curl` sends it for large bodies.
  - Deadlines: 10 s total to receive the whole head, 30 s total to receive the body, 30 s idle between requests, 30 s per response write. Total deadlines, not per-read timeouts, so a slow trickle cannot hold a thread.
  - HTTP/1.1 keep-alive. HTTP/1.0 closes.
  - The client half writes one request and reads one `Content-Length` response, over `TcpStream` or `UnixStream`.
- `outcome`: the merge rule above as one small state type, shared by `zerodb mcp` and the Rust test client.
- Tests: inline `#[cfg(test)]` table tests for the framing code and the outcome merge, because both are private logic that is hard to reach from outside (TESTING.md rule 4). One framing table covers each accept and reject row above. One outcome table covers each row of the merge table. One proptest: random bytes never panic the framing and never yield a body past the cap. Config and auth are tested through the binary in T3.
- Done when the tables and proptest pass and clippy is clean.

## T3: `zerodb serve`, listeners and the HTTP API

- Instance id: 16 random bytes, hex, made at startup. `TapeOptions::server_address` carries `instance=<id> unix=<path> tcp=<addr or none>`, so the tape writes it into `LOCK`. `/healthz` returns the same id. Clients that found the server through `LOCK` compare the two.
- Startup order. This order makes the lock race and stale sockets safe:
  1. Load and check the config.
  2. TCP. `--listen` defaults to `127.0.0.1` on a fixed port chosen in this task and documented in the README. `--listen none` binds no TCP. Port 0 is allowed. A non-loopback address fails startup: "non-loopback listening needs TLS, which zerodb does not support yet; put a TLS proxy on loopback in front".
  3. Open the engine. On `DataDirLocked` print the holder line and exit with code 75. Recovery happens here, so this step can take a while.
  4. Unix socket, only now that the lock is held. Default path `<data_dir>/zerodb.sock`.
     - Check the length of both the final path and the staging bind path against `sun_path` (104 bytes on macOS, 108 on Linux), counting the terminating NUL. Fail with a hint to set `--unix-socket`.
     - Check the parent: it exists, is a directory, is owned by `geteuid()`, and is not writable by group or others.
     - If the path exists and is a socket, connect to it. A successful connect means another server owns it: fail. A refused connect means a stale socket from a crash: remove it. If the path exists and is not a socket: fail.
     - Under the lock, remove any leftover `<parent>/.zerodb-sock.<socket file name>.*` directories owned by `geteuid()` (a kill before cleanup leaves them, and PIDs repeat across sidecar restarts). Scoping by the socket file name leaves other servers' staging directories in a shared parent alone. Create a private staging directory `<parent>/.zerodb-sock.<socket file name>.<instance id>` with mode 0700. Bind the socket inside it. Set mode 0600, or with `--unix-socket-group` set the group with `std::os::unix::fs::chown` and mode 0660. Then `hard_link` the staged socket onto `<path>`, which fails if `<path>` exists, and remove the staged name and directory. Only the owner can reach the socket before its mode is set, whatever the umask, and an existing path is never clobbered.
  5. Start the accept threads. Print the listeners and the instance id to stderr.
- `--exit-on-poison`: after any write, and on a 1 s tick, check `Engine::health()`. When it is `Poisoned`, stop accepting, finish responses already being written, remove the socket path while still holding the lock, and exit with code 70. The lock is released on exit. The next start replays the tape. Without the flag the server keeps serving reads and 503 `poisoned` (D38), and the operator restarts it. This is the recovery path for a poisoned server either way: a fresh process opens the directory and recovery decides each unsynced write.
- Connections: one thread per connection, capped (default 256). Over the cap: 503 `busy` and close. The engine is shared as `Arc<Engine>`. Concurrent appends from many connection threads land in the same writer groups.
- `Host` rule: on TCP the `Host` header must be `127.0.0.1`, `localhost` or `[::1]`, with an optional port. Missing or anything else gets 403. On the Unix socket `Host` is ignored, present or not.
- Routes:
  - `POST /v1/append`. Body `{"agent", "session", "idem_key", "turns": [{"role", "content", "meta"?}]}`. `role` is `system`, `user`, `assistant` or `tool`. `content` is a UTF-8 string. `meta` is a JSON object, stored as its compact text (64 KiB limit). `idem_key` is required, 1 to 128 bytes.
  - `GET /v1/window?agent=&session=&last=N&max_bytes=B` or `...&since_seq=S&limit=N`. `last` and `limit` are capped at 1,000. `max_bytes` defaults to 1 MiB and is capped at 8 MiB. Response `{"turns": [{"seq", "lsn", "commit_time_ns", "role", "content", "meta"}], "truncated": bool}`. Content that is not UTF-8 (only possible from direct Rust API writes) comes back with U+FFFD and `"lossy": true` on that turn.
  - `GET /healthz`. No token. 200 `{"status": "ok", "instance"}`, 200 `{"status": "read_only", "instance"}`, 503 `{"status": "poisoned", "instance"}`. Read-only stays 200 on purpose, as 0003 says.
- Write responses. The status follows the outcome first, then the error kind:

  | Engine result | Status | Body |
  |---|---|---|
  | `Appended` | 200 | `{"outcome": "committed", "deduplicated", "txn", "first_lsn", "last_lsn", "seqs"}` |
  | Any error with `Outcome::Committed` | 200 | `{"outcome": "committed", "receipt": null, "error": <kind>}` |
  | `InvalidInput`, bad JSON, missing `idem_key` | 400 | `{"outcome": "not_committed", "error": "invalid_input", "message"}` |
  | No or bad token | 401 | `{"outcome": "not_committed", "error": "unauthorized"}` |
  | Bad `Host` on TCP | 403 | `{"outcome": "not_committed", "error": "forbidden_host"}` |
  | `IdempotencyConflict` | 409 | `{"outcome": "not_committed", "error": "idempotency_conflict"}` |
  | Body over cap | 413 | `{"outcome": "not_committed", "error": "too_large"}` |
  | `Storage(BatchTooLarge)` (over 4 MiB or 1,024 records) | 413 | `{"outcome": <engine outcome>, "error": "too_large"}` |
  | `ResourceExhausted(r)` | 429 | `{"outcome": "not_committed", "error": "resource_exhausted", "resource": r}` |
  | `Storage(DiskFull)` | 507 | `{"outcome": <engine outcome>, "error": "disk_full"}`. Usually `not_committed`. A cleanup or sync failure after a space error returns `unknown` (`writer.rs` reject path), and the body must say so |
  | `Poisoned` | 503 | `{"outcome": <engine outcome>, "error": "poisoned"}` |
  | Any other storage error | 500 | `{"outcome": <engine outcome>, "error": "storage"}` |

- Logging: one stderr line per request with method, path, status, tenant name and latency. Never the `Authorization` header, never a body, never a query string (it holds session names). Errors from config parsing never echo file content.
- Also wire `zerodb recover --truncate-at <lsn> --data-dir <dir>` to `Tape::recover_truncate_at`. The README promised it with the server binary.
- `serve` is a library function `serve::run(engine: Arc<Engine>, listeners, config)`, so tests can pass an engine built on `SimDisk`.
- Tests in `crates/zerodb/tests/it/` (`main.rs`, `support.rs`, `api.rs`, `listen.rs`). One scenario per path, many assertions:
  - `api::append_window_and_retry_over_both_listeners`: for each listener (TCP, Unix), append, read the window right after the ack and see the turn (I4), retry the same key (`deduplicated: true`, same txn and seqs), retry with a changed payload (409), then window with a `max_bytes` table. Drop the server, reopen the engine on the same directory, retry the first key: `deduplicated: true` with the original receipt.
  - `api::auth_and_host_rules`: one table. Rows: no token, bad token, token of tenant B reading tenant A's session (empty window, never A's data, I11), good token. A bad token with an 8 MiB body declared gets 401 and a closed connection with the body unread. Host rows: loopback names pass on TCP, `evil.example` gets 403 on TCP, the same `evil.example` and a missing `Host` pass on Unix. This Unix row is the Firecracker guest case.
  - `api::health_tracks_engine_state`: engine on `SimDisk`. Ok gives 200 ok with the instance id. A capacity limit under the floor makes an append return 507 `not_committed` and health 200 `read_only`, while windows still serve. Freeing space and retrying the same key commits once. An injected sync failure during an append returns 500 `storage` with `outcome: "unknown"`, because the engine reports it as `Storage(SyncFailed)`, and health turns 503 `poisoned`. A batch over 4 MiB but under the 8 MiB body cap returns 413 `too_large`. A retry of that key now returns `not_committed` from the poisoned writer, because a failed sync publishes no dedup entry. This is the case the outcome rule exists for. A receipt failure after commit returns 200 `committed` with `receipt: null`.
  - `listen::startup_checks`: one table driving the real binary. Rows: config with mode 0644 fails, and stderr does not contain the token; `--token-file` with mode 0644 fails the same way for `zerodb mcp`; `--listen 0.0.0.0:0` fails with the TLS message; socket mode is 0600; with `--unix-socket-group <own gid>` it is 0660 with that group; a stale socket from a killed server is replaced; a live socket owned by another server fails startup; a regular file at the path fails; a group-writable parent fails; a path past `sun_path` fails with the hint. A second `serve` on the same data dir exits with code 75 and leaves the first server's socket working. With `--exit-on-poison` and the `fault-inject` trigger, a write gets `unknown`, the server exits 70, and the socket path is gone.
- Done when all four tests pass on Linux and macOS.

## T4: `zerodb mcp`

- Stdio adapter. Newline-delimited JSON-RPC 2.0 on stdin and stdout. Logs to stderr only. When stdin closes, it finishes the call in progress and exits 0.
- MCP version. I am choosing the legacy `initialize` handshake (revision `2025-11-25`) over dual-era support because the 2026-07-28 stdio binding tells dual-era clients to fall back to `initialize` on any error that is not a recognized modern error ([stdio binding](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio), "Backward Compatibility"). That covers today's clients and dual-era ones.
  - On `initialize`, echo the client's version if it is `2025-11-25`, `2025-06-18`, `2025-03-26` or `2024-11-05`, else answer `2025-11-25`.
  - `ping` is answered before and after `initialize`. The 2025-11-25 lifecycle allows `ping` before initialization completes.
  - Every other pre-`initialize` request, including `server/discover`, gets `-32601`.
- Methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`. `notifications/cancelled` is ignored: a cancelled write may still commit, and that is safe because it carries a key.
- Tools. The model never sees keys, tokens or tenants.
  - `memory_append {content, role? = "assistant", session?, meta?}`. Returns text `committed seq=<n>` and `structuredContent {seq, lsn, deduplicated}`.
  - `memory_recent {n? = 20 (max 200), session?, max_bytes? = 64 KiB}`. Returns the turns as text and as `structuredContent`.
  - `agent` comes from `--agent` (default `mcp`). `session` defaults to `--session` (default `default`). A tool may pass `session`. It stays inside the token's tenant, so I11 holds.
  - Errors return `isError: true` with the merged outcome in the text. `not_committed` says nothing was written. `unknown` says the write may be saved and repeating the call may duplicate it.
- Connecting (`launch` module). Inputs: `--url unix:///path` or `--url http://127.0.0.1:<port>`, `--data-dir`, `--config`, and the token from `ZERODB_TOKEN`, `--token-file` or the config entry for `--tenant`.
  1. If `--url` is set, use it. An operator chose it, so no instance check. This is the guest path.
  2. Else read `<data_dir>/LOCK` and parse `instance` and `unix`. Discovery uses the Unix socket only. The TCP address in `LOCK` is never auto-trusted, because the instance id is public through `/healthz` and any local user can bind a stale port and echo it. Before connecting, check the socket path with `lstat`: a socket owned by `geteuid()`, under a parent owned by `geteuid()` that is not writable by group or others. Then probe `GET /healthz` and trust the server only if it answers 200 or 503 with the `instance` from `LOCK`. A failed check, a refused connect, a timeout or a different id means a stale `LOCK` or a foreign listener. Never send the token to it. TCP is used only when an operator passes `--url`.
  3. Else spawn `zerodb serve --config <path> --data-dir <resolved data dir> --listen none --exit-on-poison`. The resolved directory is always passed, so a `--data-dir` that overrides the config is the one the server opens and the one the adapter polls. Unix socket only, never a TCP port, so two racing launchers can only collide on the lock: the loser exits 75, never with `EADDRINUSE`. Stdin is null. Stdout and stderr append to `<data_dir>/serve.log`. `process_group(0)` lets it outlive the adapter. The token is never on the command line.
  4. Repeat step 2 until a server is trusted or `--start-timeout` (default 60 s) passes. A spawned server that exits 75 lost the lock race. That is expected: the winner is recovering or binding. But the holder may also be an `--exit-on-poison` server that is shutting down, and every replacement can exit 75 before it lets go. So when the last spawned child has exited and no trusted server is reachable, spawn again with backoff (100 ms doubling to 2 s) until the deadline.
- One `idem_key` per tool call: 16 random bytes, hex. Every retry of that call reuses it.
- Retry rule, shared with the Python client: retry on a transport error (connect, reset, timeout, short or malformed response) and on `outcome: unknown`. Merge results with the outcome rule. Backoff 50 ms doubling to 2 s with jitter, until `--retry-deadline` (default 30 s). On a transport error, rerun the connect steps first. A killed or poison-exited server is then restarted by whichever adapter notices first.
- Tests in `tests/it/mcp.rs`:
  - `mcp::tool_calls_over_stdio`: spawn `zerodb mcp` against a running test server. Drive `ping` before `initialize`, `initialize` with `2024-11-05` and with an unknown version, `tools/list`, two `memory_append` calls and `memory_recent`. Assert the version echo and fallback, tool schemas, `-32601` for a pre-initialize `server/discover`, seqs, and that no `idem_key` appears in any message the model sees. Close stdin and assert exit 0.
  - `mcp::launch_race_starts_one_server`: start two adapters at once on an empty data dir with no server. Exactly one `serve` holds the lock. The other exits 75, never `EADDRINUSE`. Both adapters' first `memory_append` commits. Then replace `LOCK`'s address with a socket served by a fake listener that answers `/healthz` with a different instance id. A fresh adapter refuses it and the fake listener never receives an `Authorization` header. A second fake listener on the stale TCP address echoes the correct instance id: the adapter never connects to it. A socket owned by another user or under a group-writable parent is refused before any request. A poisoned `--exit-on-poison` server that is slow to exit while a fresh adapter launches: the adapter respawns after the losers exit 75 and its first `memory_append` commits once the holder is gone. A leftover `.zerodb-sock.<socket file name>.*` staging directory from a killed server does not block startup. Last row: a config naming directory A and `--data-dir B` starts the server on B, and the adapter connects to it.
  - `mcp::unknown_outcome_never_downgrades`, `fault-inject` build. Two rows:
    - Auto-started server: create the trigger, call `memory_append`. The server answers `unknown` and exits 70. The adapter relaunches it, retries with the same key, and returns `committed` with `deduplicated: true`. The written bytes survived the process exit, so only the same key can explain a single copy and the dedup flag. Exactly one copy in the window.
    - Server started by the test without `--exit-on-poison`, adapter with `--url`: create the trigger, call `memory_append`. The first answer is `unknown`, retries get `not_committed` from the poisoned server. The test restarts the server after 1 s. The tool returns `committed`, `deduplicated: true`, one copy. With `--retry-deadline 500ms` and no restart, the tool returns `unknown`, never `not_committed`.
- Done when all three pass.

## T5: Python client

- `clients/python/zerodb_client.py`, one file, standard library only (`http.client`, `socket`, `json`, `secrets`, `time`, `random`).
- `Client(url, token)`. URL forms: `http://127.0.0.1:<port>`, `unix:///path`, and `vsock://<cid>:<port>`. The vsock form uses `socket.AF_VSOCK` and is guarded with `hasattr(socket, "AF_VSOCK")`. Where it is missing (macOS, Windows, old Linux builds) the constructor raises a clear `ValueError`.
- `append(agent, session, turns, idem_key=None)` creates one key per call when none is given, retries per T4, and merges with the outcome rule. Returns the decoded receipt including `deduplicated`, or a receipt with `receipt_missing: true`. Raises `NotCommitted` only when no attempt was unclear. Raises `OutcomeUnknown` otherwise.
- `window(agent, session, last=None, since_seq=None, limit=None, max_bytes=None)` and `health()`.
- Never puts the token in an exception message or `repr`.
- Tests (`clients/python/tests/conftest.py`, `helpers.py`, `test_client.py`): a fixture starts `$ZERODB_BIN serve` on port 0 and a socket in pytest's `tmp_path`.
  - `test_append_retry_and_window` parametrized over `http` and `unix`: append, retry with the same key, window with `max_bytes`.
  - `test_retry_survives_server_restart`: send an append with a fixed key and SIGKILL the server right after the request is written, before the reply. Restart, call again with the same key. Exactly one copy in the window.
  - `test_unknown_outcome_never_downgrades`, parametrized like the second row of `mcp::unknown_outcome_never_downgrades`: trigger file, a restart after 1 s gives `committed` with `deduplicated: True`; no restart with a short deadline raises `OutcomeUnknown`, never `NotCommitted`.
  - vsock is not tested in CI. It is covered by the docs page.
- Done when pytest passes against the `fault-inject` release binary on Linux and macOS.

## T6: the done criteria, end to end

- `tests/it/durability.rs`, `#[ignore = "spawns processes and kills the server"]`, run in the `--include-ignored` CI step.
- Kills are tied to sends, not to timers. A test-side Unix socket proxy stands in for Firecracker's vsock forwarding. It accepts on `<dir>/v.sock_5252`, and for each connection opens a new connection to the server socket and copies bytes both ways. After it has forwarded the full body of request number N (N from a seeded RNG, printed on failure), it sends SIGKILL to the server.
- `durability::two_mcp_clients_and_a_killed_server_lose_nothing`:
  - Client H is a host agent: `zerodb mcp --data-dir D --config C`. It can start the server.
  - Client G simulates a Firecracker guest: `zerodb mcp --url unix://<dir>/v.sock_5252`, through the proxy. It never sees `D` and cannot start a server. Its requests carry a non-loopback `Host`, as a guest's would.
  - Both start before any server exists, so H exercises the launch path.
  - Each sends 500 `memory_append` calls with unique content, from 2 threads each. The proxy kills the server twice, each time right after forwarding a request.
  - H restarts the server on its next transport error. G retries through the proxy until it is back.
  - Oracle, read back over HTTP:
    - every call that returned `committed` appears exactly once;
    - every call that returned `unknown` appears at most once;
    - zero calls returned `not_committed` for content that is present after restart;
    - seqs per session are dense;
    - the proxy confirms both kills happened after a forwarded request.
- `durability::killed_server_restarts_with_every_acknowledged_write`: a plain Rust HTTP client writes from 8 threads. One thread writes a request, then the test sends SIGKILL before that thread reads the reply. Restart and compare against the acknowledged set. Same oracle. This is the 0003 done criterion without MCP in the way.
- CI: add the `--include-ignored` step and the pytest step to `.github/workflows/ci.yml`, on both OS rows. Pin `pytest` by exact version in the step.
- Done when both scenarios pass 20 runs in a row locally on macOS and once in CI on both OS rows.

## T7: docs and decision

- `docs/deploy/sandboxes.md`, per owner decision 2:
  - Where to run: on the host or in a sidecar. Never inside a disposable sandbox.
  - Firecracker vsock. A guest connects to CID 2 (the host) and port `P`. Firecracker forwards that connection to the host Unix socket `<uds_path>_P` ([vsock.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md), "Guest-Initiated Connections"). Ports from 2^30 up are reserved for host-initiated connections.
  - Without the jailer: `zerodb serve --unix-socket /run/zerodb/zerodb.sock`, then a symlink `<uds_path>_<port>` pointing at it. One server serves every VM that has such a symlink.
  - With the jailer. It builds a chroot per VM under `<chroot_base>/<exec_file_name>/<id>/root`, enters a new mount namespace, `chown`s the chroot to the VM's `--uid` and `--gid`, and drops to them ([jailer.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md)). So:
    - bind-mount the server's socket directory into each jail before starting the jailer;
    - inside each jail, symlink `<uds_path>_<port>` to the socket in that mount;
    - start the server with `--unix-socket-group <gid>`, start each jailer with `--gid` set to that group, and give each VM its own `--uid`;
    - one server serves many VMs through the same bind mount;
    - never hard-link or bind-mount the socket file itself. A server restart installs a new socket inode, and a link or file mount would keep pointing at the dead one.
  - In the guest: the Python client's `vsock://2:<port>`, or for `zerodb mcp` a bridge: `socat UNIX-LISTEN:/run/zerodb.sock,fork VSOCK-CONNECT:2:<port>` and `zerodb mcp --url unix:///run/zerodb.sock`.
  - Containers, gVisor, Kata: mount the socket's directory into the sandbox and use `unix://`. The connecting process needs the server's uid, or membership in `--unix-socket-group`.
  - Durability warning. Firecracker's block `cache_type` defaults to `Unsafe`, which does not advertise flush, so zerodb inside such a guest can lose acknowledged writes on a host crash. Use `Writeback` if it must run inside ([block-caching.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/api_requests/block-caching.md)). gVisor's default `--overlay2=root:self` keeps root filesystem writes in an overlay, not the host filesystem ([gVisor filesystem guide](https://gvisor.dev/docs/user_guide/filesystem/)). They do not outlive the sandbox.
  - No container image yet.
- Root `README.md`: "What exists" gains the server, MCP and Python client. A quick start: `zerodb init`, `zerodb serve`, one `curl` append and window over TCP and over `--unix-socket`, and an MCP client config snippet. Remove "CLI wiring arrives with the server binary in S3".
- `crates/zerodb/README.md`: the API table from T3, the outcome rule, the connect steps, exit codes (75 lock held, 70 poisoned with `--exit-on-poison`), `--unix-socket-group` and its `/etc/group` limit, and that an auto-started server keeps running after its adapters exit.
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

**A2. Invariants table.** Add I15 after I14. Update the "Checked by" cells of I4, I6, I7, I11 and I14.

```markdown
| I15 | Only a token picks a tenant. TCP serves loopback `Host` names only and refuses non-loopback binds. The Unix socket is mode 0600, or 0660 with an explicit group, under a parent only the owner can write. Only the lock holder may replace its path. Clients never send a token to a discovered server unless it is a Unix socket owned by the same user under an owner-only parent and its instance id matches `LOCK`. TCP is only used when given explicitly | Bind order: TCP, lock, then socket. Bind in a 0700 staging directory, chmod, hard link. Constant-time BLAKE3 compare over every token. Auth before the body is read | `zerodb --test it`: `api::auth_and_host_rules`, `listen::startup_checks`, `mcp::launch_race_starts_one_server` |
```

- I4 add: `zerodb --test it`: `api::append_window_and_retry_over_both_listeners`.
- I6 add: `api::append_window_and_retry_over_both_listeners`, `mcp::unknown_outcome_never_downgrades`, `durability::two_mcp_clients_and_a_killed_server_lose_nothing`; Python `test_unknown_outcome_never_downgrades`.
- I7 add: `mcp::launch_race_starts_one_server`, `listen::startup_checks`.
- I11 add: `api::auth_and_host_rules`.
- I14 add: `api::health_tracks_engine_state`.

**A3. S3 section.** Replace the first two bullets with:

```markdown
- `zerodb serve` holds the lock. HTTP/JSON on TCP bound to 127.0.0.1 by default. Other addresses need an explicit flag and TLS. S3 has no TLS, so it refuses them. A TLS proxy on loopback covers remote access.
- It also listens on a Unix domain socket, default `<data_dir>/zerodb.sock`, set by flag or config. The socket file is mode 0600, or 0660 with `--unix-socket-group`. Its parent must be owned by the server's user and not writable by group or others. Only the lock holder may remove a stale socket. Agents in sandboxes and microVMs reach the host through it: Firecracker forwards guest vsock connections to a host Unix socket, and containers, gVisor and Kata can share it through a volume. See `docs/deploy/sandboxes.md`.
- Auth: one token per tenant in a config file that must be mode 0600. The token travels only in the `Authorization` header with the Bearer scheme and is compared in constant time. The tenant comes from the token, never from the request. On TCP the `Host` header must be a loopback name. On the Unix socket `Host` has no meaning and is not checked.
- `LOCK` and `/healthz` carry a per-start instance id. Clients that find the server through `LOCK` trust it only when the ids match.
- `--exit-on-poison` makes a poisoned server exit and release the lock. `zerodb mcp` always sets it when it starts a server.
```

In the outcome bullet, replace "whether it was deduplicated" with "a `deduplicated` flag", and add: "A client never downgrades an outcome. After a transport error or `unknown`, only `committed` replaces `unknown`." Add to the `GET /v1/window` bullet: "Turns are never cut. The response says when older turns were left out." In the `zerodb mcp` bullet, add: "An auto-started server listens on the Unix socket only." Add to the done bullet: "An MCP client in a simulated Firecracker guest, which only reaches the Unix socket, loses nothing either."

**A4. Decisions table at the end of 0003.** Add:

```markdown
| Unix socket in S3? | Yes. It is how sandboxed and microVM agents reach the host (owner, 2026-10-21, D75) |
| Run zerodb inside a sandbox? | No. Host or sidecar. A guest's storage may drop flushes (Firecracker `Unsafe`) or vanish (gVisor overlay) |
| TLS in S3? | No. Non-loopback TCP is refused. A loopback TLS proxy covers remote access |
```

## D75 text

Under "Slim v0.1 (0003)", after D74:

```markdown
| D75 | S3 serves HTTP on loopback TCP and on a 0600 Unix socket (0660 with an explicit group), with the same token auth. The `Host` check applies to TCP only. Non-loopback TCP is refused until TLS exists. zerodb runs on the host or in a sidecar, never in a disposable sandbox. No container image in S3. New crates: `serde_json =1.0.154` (defaults off, `std`, `Value` only) and `httparse =1.10.1` (defaults off). `rustix =1.1.2` gains `process` for `geteuid`. HTTP framing is std-only. Clients never downgrade an outcome | Guests cannot reach host loopback. Firecracker vsock and shared volumes both end at a host Unix socket, so one listener covers them. Guest storage can drop flushes or vanish. `hyper` needs an async runtime and `tiny_http` is unmaintained and still needs a client. A `not_committed` after an unclear attempt can hide a durable write |
```

## Risks

| Risk | What could go wrong | Mitigation in this plan |
|---|---|---|
| Lock race between `zerodb mcp` launchers | Two adapters each start a server. Both write, or one unlinks the other's socket | Only the engine lock decides. Auto-started servers bind no TCP, so the loser fails on the lock with exit 75. The socket is bound only after the lock. Adapters poll `LOCK` and check the instance id. Tested in `mcp::launch_race_starts_one_server` and T6 |
| Stale `LOCK` or a foreign listener | An adapter trusts whatever answers at an old address and sends it the token | Instance id in `LOCK` and `/healthz`. No match, no token |
| Winner still recovering | Open time is O(log size) (I13). Pollers give up before the server answers | `--start-timeout` (60 s default). The timeout error names `serve.log`. Docs state open time grows with the log |
| Outcome downgrade | A retry after `unknown` gets `not_committed` from the floor or the poisoned fast path, and the client says nothing was written while the first attempt is durable | The outcome rule. Only `committed` replaces `unknown`. Tested in T3, T4, T5 and T6 |
| Committed without a receipt | A write is durable but the reply has no seqs. A client retries forever or reports failure | 200 `committed` with `receipt: null`. Clients never retry it |
| Stale socket after a crash | `kill -9` leaves the socket file. A restart cannot bind | The lock holder probes it. Refused means stale: remove and rebind. A live peer or a non-socket file fails startup |
| Socket permission window | Someone connects between `bind` and `chmod`, or the install clobbers a path | Bind a temp name, set the mode, `hard_link` onto the final path (fails if it exists), remove the temp name. Parent ownership check with `geteuid` |
| Token leakage | Tokens show up in logs, panics, `ps`, `Debug` output, or reach a foreign listener | Never log headers, bodies or queries. Redacted `Debug`. Tokens never on a command line. Config errors never echo content. Instance id check before sending a token. Python client hides the token from `repr` and exceptions |
| Config swapped during the check | A check on the path passes, then a different file is read | Open once, `fstat` the handle, read from the same handle |
| Idempotency across restart | A retry after a restart writes twice | The `idem_key` is in the turn payload. Recovery and checkpoints rebuild the dedup table (I6, I8). Tested by the reopen rows in T1 and T3 and by T4 to T6. A retry after the key leaves the bounded window writes again. Documented |
| Retries exhausted | The model calls again after an `unknown`, with a new key, and duplicates a turn | The tool error says the write may be saved and that repeating may duplicate. The 30 s deadline is far below the dedup window. Residual risk is documented |
| Poisoned server holds the lock | It serves reads and 503 forever, so no adapter can start a fresh one | `--exit-on-poison`, always set by `zerodb mcp` on auto-start. Exit 70 releases the lock. The next transport error relaunches it, and recovery decides unsynced writes. Manual servers without the flag need an operator restart, as documented |
| Jailer uid per VM | Each jailed Firecracker runs as its own uid and cannot connect to a 0600 socket | `--unix-socket-group` with mode 0660. Docs cover the bind mount and the symlink |
| Group names from NSS | `--unix-socket-group <name>` cannot resolve an LDAP group without `libc` | Accept a numeric gid. Documented |
| `sun_path` length | Deep data directories exceed 104 or 108 bytes | Startup fails with a hint to set `--unix-socket` |
| Slow or idle clients | Connections pile up and starve threads | Total head and body deadlines, idle and write timeouts, auth before the body, a connection cap with 503 `busy` |
| HTTP subset gaps | A real client sends something the subset rejects | `httparse` for the head. Rejections are explicit status codes. `Expect: 100-continue` handled. Python and Rust clients tested. A `curl` example in the README is run by hand at review |
| Orphan servers | An auto-started server lives on after every adapter exits | Documented, with how to stop it. It holds no unsynced acknowledged state, so `kill` is always safe |
| `fault-inject` leaks into a release | A sync can be failed by creating a file | The feature is off by default. CI builds it only for test steps. The release binary is built without it |

## Open questions for the owner

None left from round 1. The answers are in the table at the top.
