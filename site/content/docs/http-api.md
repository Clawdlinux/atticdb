`atticdb serve` runs the server. It speaks HTTP/JSON over loopback TCP and a Unix socket, with the same token auth on both. `atticdb mcp` and the shell verbs talk to it.

## Run a server by hand

```sh
cargo build -p atticdb --release
export PATH="$PWD/target/release:$PATH"
TOKEN=$(atticdb init --config atticdb.json --tenant me)   # 0600 config, token printed once
atticdb serve --config atticdb.json --data-dir ./data &    # TCP 127.0.0.1:7425 and ./data/atticdb.sock
curl -s -H "Authorization: Bearer $TOKEN" \
  -d '{"agent":"planner","session":"run-1","idem_key":"k1","turns":[{"role":"user","content":"hi"}]}' \
  http://127.0.0.1:7425/v1/append
curl -s --unix-socket data/atticdb.sock -H "Authorization: Bearer $TOKEN" \
  'http://localhost/v1/window?agent=planner&session=run-1&last=10'
```

That appends over TCP and reads over the Unix socket. A repeated `idem_key` returns `"deduplicated": true`.

Facts live at `/v1/facts/put`, `get`, `list` and `delete`. Each key holds 1 current value, and a later put replaces it. With `"if_lsn": <lsn>`, a put writes only if the current value still has that LSN. A mismatch is 409 `conflict`.

## atticdb serve

- TCP listens on `127.0.0.1:7425` by default. `--listen <addr>` picks another loopback address, port 0 included. `--listen none` binds no TCP.
- A non-loopback address fails startup, because AtticDB has no TLS. Put a TLS proxy on loopback in front.
- The Unix socket defaults to `<data_dir>/atticdb.sock`. Set it with `--unix-socket`. The socket is mode 0600. `--unix-socket-group <gid or name>` makes it 0660 with that group.
- For sandboxes, put the socket in its own directory. See [sandboxes.md](https://github.com/Clawdlinux/atticdb/blob/main/docs/deploy/sandboxes.md).
- The data directory is created with mode 0700. An existing one must be owned by the server's user with no group or other bits, or startup fails.
- At most 256 connections. Past that, a new connection gets 503 `busy`. A request body can be up to 8 MiB.
- `--exit-on-poison`: when the engine is poisoned, the server stops accepting, finishes responses in flight and exits 70. A fresh start recovers. Without it, a poisoned server serves reads and 503 `poisoned` until you restart it.
- Logs: startup, shutdown and 5xx answers on stderr, one line each. `--log-requests` adds a line for every request. Never headers, bodies or query strings.
- Stop it with a signal. Every acknowledged write is already synced, so `kill` is safe.

| Exit code | Meaning |
|---|---|
| 1 | Startup failed: config, listener or socket checks |
| 2 | Usage error |
| 70 | Poisoned, with `--exit-on-poison` |
| 75 | Another process holds the data directory lock |

On Windows the server listens on loopback TCP only. See [Windows](docs:windows).

## HTTP API

Send `Authorization: Bearer <token>`. The token picks the tenant. On TCP the `Host` header must be `127.0.0.1`, `localhost` or `[::1]`, with an optional port. On the Unix socket `Host` is not checked.

| Route | Request | 200 response |
|---|---|---|
| `POST /v1/append` | `{"agent", "session", "idem_key", "turns": [{"role", "content", "meta"?}]}` | `{"outcome": "committed", "deduplicated", "txn", "first_lsn", "last_lsn", "seqs"}` |
| `GET /v1/window` | `?agent=&session=` plus `last=N`, or `since_seq=S` with an optional `limit=N`. Optional `max_bytes=B` | `{"turns": [{"seq", "lsn", "commit_time_ns", "role", "content", "meta"}], "truncated"}` |
| `GET /v1/search` | `?q=` words, plus optional `limit=N` (default 10, cap 100), `before=LSN` and `max_bytes=B` | `{"hits": [{"kind": "turn", "agent", "session", "seq", "lsn", "commit_time_ns", "role", "content", "meta"} or {"kind": "fact", "key", "lsn", "commit_time_ns", "agent", "content", "meta"}], "next"}` |
| `GET /v1/sessions` | Optional `limit=N` (default 100, cap 1,000), and `after_agent=` with `after_session=` | `{"sessions": [{"agent", "session", "turns"}], "next": {"agent", "session"} or null, "total": {"sessions", "turns"}}` |
| `GET /v1/records` | Optional `before=LSN`, `limit=N` (default 50, cap 1,000), `kind=turn` or `kind=fact`, and `max_bytes=B` | `{"records": [{"kind": "turn", "lsn", "commit_time_ns", "agent", "session", "seq", "role", "content", "meta"} or {"kind": "fact", "lsn", "commit_time_ns", "key", "agent", "deleted", "content", "meta"}], "next"}` |
| `GET /healthz` | No token | `{"status": "ok" or "read_only", "instance"}`. 503 with `"poisoned"` |
| `GET /v1/stats` | Unix socket only, no token | One flat object: `instance`, `health`, `durable_lsn`, tenant and fact counts, writer groups, checkpoint and lock timings, tape-index memory. Counters cover this open |
| `GET /metrics` | Unix socket only, no token | The same numbers in Prometheus text format 0.0.4: counters as `atticdb_<name>_total`, gauges as `atticdb_<name>`, `_ns` maxima as `_seconds`, `atticdb_read_only`, the `atticdb_append_ack_seconds` histogram of committed appends (50 µs to 1 s), and the `atticdb_turn_content_bytes` histogram of new turns (64 B to 4 MiB). `turn_repeats` counts new turns whose content matches one of about 4,096 recent hashes, a lower bound on exact repeats |

`/v1/stats` and `/metrics` count activity across every tenant, so no tenant token can read them. Whoever can open the socket can. On TCP they are 404 behind the token.

- `role` is `system`, `user`, `assistant` or `tool`. `meta` is a JSON object, at most 64 KiB. `idem_key` is required, 1 to 128 bytes.
- `last` and `limit` are capped at 1,000. `max_bytes` defaults to 1 MiB and is capped at 8 MiB. Turns are never cut. `truncated` says the budget, not the query, ended the window.
- Search walks records newest first. A turn matches when its content holds every word of `q`, ignoring case, as a substring. A plural word also matches its singular, so `keys` finds `key`.
- A fact matches on its key or content, and only its current value counts. A call visits at most 1,000,000 records. Pass `next` as `before` to go on. `next` is null once the walk reaches the oldest record.
- Sessions lists the tenant's sessions with at least 1 published turn. `total` counts all the tenant's sessions and turns, not only this page. Pass `next` as `after_agent` and `after_session` to go on.
- Records walks the tenant's turns and fact revisions newest first, each as written. It is the log, not the current state. A delete has `"deleted": true` and empty content.
- Read routes that need a token answer 400 `invalid_input` to an unknown query parameter.
- `Transfer-Encoding` gets 501. `Expect: 100-continue` works.

### Write errors

Every body carries `outcome`:

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
- A transport error or `unknown`: retry with the same key. From then on only `committed` replaces `unknown`. A later `not_committed` does not prove the first attempt failed. It can come from a poisoned or full writer that never looked at the key.
- A call whose attempts all sent no bytes reports `not_committed`. An attempt sends nothing when connect fails or under 2 s are left.
- Retries back off from 50 ms, doubling to 2 s with jitter, until the retry deadline. That is 30 s by default, `--retry-deadline` in `atticdb mcp`.
- A retry after the key leaves the dedup window writes again.

`atticdb mcp` and the [Python client](https://github.com/Clawdlinux/atticdb/blob/main/clients/python/README.md) both follow it.

Source: [crates/atticdb/README.md](https://github.com/Clawdlinux/atticdb/blob/main/crates/atticdb/README.md) and [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md)
