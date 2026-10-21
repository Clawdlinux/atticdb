# zerodb

The server binary. HTTP/JSON over loopback TCP and a Unix socket, with the same token auth on both.
`zerodb mcp` is a stdio MCP adapter that talks to it.

```sh
zerodb init --config zerodb.json --tenant me     # writes a 0600 config, prints the token once
zerodb serve --config zerodb.json --data-dir ./data
zerodb mcp --config zerodb.json --tenant me --data-dir ./data
zerodb recover --truncate-at <lsn> --data-dir ./data
```

The config is JSON: `{"data_dir", "listen", "unix_socket", "unix_socket_group", "tenants": [{"name", "token"}]}`.
Flags override fields. It must be a regular file owned by you with no group or other bits.
Tenant names are 1 to 255 bytes. Tokens are at least 32 bytes. Duplicate names or tokens fail.

## `zerodb serve`

- TCP listens on `127.0.0.1:7425` by default. `--listen <addr>` picks another loopback address, port 0 included. `--listen none` binds no TCP. A non-loopback address fails startup, because zerodb has no TLS. Put a TLS proxy on loopback in front.
- The Unix socket defaults to `<data_dir>/zerodb.sock`. Set it with `--unix-socket`. Its parent must be owned by the server's user and not writable by group or others. The socket is mode 0600. `--unix-socket-group <gid or name>` makes it 0660 with that group. Names come from `/etc/group`. Groups from LDAP or other NSS sources need the numeric gid.
- For sandboxes, put the socket in its own directory. See [sandboxes.md](../../docs/deploy/sandboxes.md).
- The data directory is created with mode 0700. An existing one must be owned by the server's user with no group or other bits, or startup fails.
- Startup order: config, TCP, then the engine and its lock, then the Unix socket. Only the lock holder replaces a stale socket. A path with a live server or a non-socket file fails startup.
- The socket is bound in a per-start `<parent>/.zs.<8 hex>` directory, then hard-linked into place under a `flock` on `<socket>.lock`. Servers on other data directories that share the path take turns.
- A crash during that step can leave a `.zs.*` directory behind. The `.lock` file always stays. Both are harmless. A killed server also leaves its socket file. The next start on that path removes it.
- `LOCK` and `GET /healthz` carry a per-start instance id.
- At most 256 connections. Past that, a new connection gets 503 `busy`. A request body grows only as its bytes arrive, up to 8 MiB, and is dropped once parsed. Body memory stays under 256 x 2 x 8 MiB = 4 GiB in the worst case.
- `--exit-on-poison`: when the engine is poisoned, stop accepting, finish responses in flight, remove the socket path if it is still the one this server installed, and exit 70. A fresh start recovers. Without it, a poisoned server serves reads and 503 `poisoned` until you restart it.
- Logs: one stderr line per request with method, path, status, tenant and latency. Never headers, bodies or query strings.
- Stop it with a signal. Every acknowledged write is already synced, so `kill` is safe.

| Exit code | Meaning |
|---|---|
| 1 | Startup failed: config, listener or socket checks |
| 2 | Usage error |
| 70 | Poisoned, with `--exit-on-poison` |
| 75 | Another process holds the data directory lock |

## HTTP API

Send `Authorization: Bearer <token>`. The token picks the tenant. On TCP the `Host` header must be `127.0.0.1`, `localhost` or `[::1]`, with an optional port. On the Unix socket `Host` is not checked.

| Route | Request | 200 response |
|---|---|---|
| `POST /v1/append` | `{"agent", "session", "idem_key", "turns": [{"role", "content", "meta"?}]}` | `{"outcome": "committed", "deduplicated", "txn", "first_lsn", "last_lsn", "seqs"}` |
| `GET /v1/window` | `?agent=&session=` plus `last=N`, or `since_seq=S` with an optional `limit=N`. Optional `max_bytes=B` | `{"turns": [{"seq", "lsn", "commit_time_ns", "role", "content", "meta"}], "truncated"}` |
| `GET /healthz` | No token | `{"status": "ok" or "read_only", "instance"}`. 503 with `"poisoned"` |

- `role` is `system`, `user`, `assistant` or `tool`. `meta` is a JSON object, at most 64 KiB. `idem_key` is required, 1 to 128 bytes.
- `last` and `limit` are capped at 1,000. `max_bytes` defaults to 1 MiB and is capped at 8 MiB. Turns are never cut. `truncated` says the budget, not the query, ended the window.
- Content that is not UTF-8, only possible from direct Rust API writes, comes back with U+FFFD and `"lossy": true`.
- `Transfer-Encoding` gets 501. `Expect: 100-continue` works. A bad `Host`, a bad token or an oversized body gets its error before the body is read, and the connection closes.

Write errors. Every body carries `outcome`:

| Status | `error` | `outcome` |
|---|---|---|
| 200 | `receipt: null` plus the kind | `committed`. Durable, but the receipt could not be rebuilt. Do not retry |
| 400 | `invalid_input` | `not_committed` |
| 401 | `unauthorized` | `not_committed` |
| 403 | `forbidden_host` | `not_committed` |
| 409 | `idempotency_conflict` | `not_committed` |
| 413 | `too_large` | `not_committed` for an oversized body. The engine's outcome for a batch over 4 MiB or 1,024 turns |
| 429 | `resource_exhausted`, with `resource` | `not_committed` |
| 503 | `poisoned` | The engine's outcome |
| 507 | `disk_full` | Usually `not_committed`. `unknown` when cleanup after the space error failed |
| 500 | `storage` or `corrupt_index` | The engine's outcome |

## The outcome rule

Branch on `outcome`, never on the status alone. One call is one `idem_key`, reused by every retry.

- `committed` from any attempt: done.
- `not_committed` on the first attempt: nothing was written. Calling again is safe.
- A transport error or `unknown`: retry with the same key. From then on only `committed` replaces `unknown`. A later `not_committed` can come from a poisoned or full writer that never looked at the key, so it does not prove the first attempt failed.
- A call whose attempts all sent no bytes reports `not_committed`. An attempt sends nothing when connect fails or under 2 s are left.
- Retries back off from 50 ms, doubling to 2 s with jitter, until the retry deadline (30 s by default, `--retry-deadline` in `zerodb mcp`). No attempt connects or sends with less than 2 s left, the first one included. A retry deadline under 2 s sends nothing.
- A retry after the key leaves the dedup window writes again.

`zerodb mcp` and the [Python client](../../clients/python/README.md) both follow it.

## `zerodb mcp`

Newline-delimited JSON-RPC 2.0 on stdin and stdout. Logs go to stderr. It exits 0 when stdin closes.
Tools: `memory_append {content, role?, session?, meta?}` and `memory_recent {n?, session?, max_bytes?}`.
`--agent` defaults to `mcp` and `--session` to `default`. A tool may pass `session`. The tenant still comes from the token.
The token comes from `--token-file`, `ZERODB_TOKEN`, or the config entry for `--tenant`. A token file must be 0600 and yours.

How it finds a server:

1. `--url unix:///path` or `--url http://127.0.0.1:<port>`. An operator chose it, so there is no instance check. `http` takes only `127.0.0.1`, `localhost` or `[::1]`, because the token travels in cleartext.
2. Else it reads `<data_dir>/LOCK` and uses the Unix socket named there. Never the TCP address. The socket must be yours, its parent owned by you and not writable by group or others. Every directory above it must be owned by root or you and not writable by group or others unless sticky, as OpenSSH's StrictModes checks. Then `GET /healthz` must return the instance id in `LOCK`. Only then does the token go out, on that same connection.
3. Else, with `--config`, it starts `zerodb serve --config <path> --data-dir <dir> --listen none --exit-on-poison`. Output goes to `<data_dir>/serve.log`. The token is never on its command line.
4. It repeats step 2 until a server is trusted or 2 s before the call's deadline. A started server that exits 75 lost the lock race. If no trusted server shows up, it starts another, backing off from 100 ms to 2 s.

An auto-started server runs in its own process group and outlives the adapters. Stop it with `kill <pid>`. The pid is in `<data_dir>/LOCK`.

## Tests

Socket paths, staging included, must fit `sun_path` (104 bytes on macOS). Tests put them under `target/s`. Set `ZERODB_TEST_DIR` to a short directory if your checkout path is too deep.
