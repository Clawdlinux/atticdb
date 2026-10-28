# zerodb

The server binary. HTTP/JSON over loopback TCP and a Unix socket, with the same token auth on both.
`zerodb mcp` is a stdio MCP adapter that talks to it.

```sh
zerodb init --config zerodb.json --tenant me     # writes a 0600 config, prints the token once
zerodb serve --config zerodb.json --data-dir ./data
zerodb mcp                                       # no flags: the per-user home
zerodb connect vscode                            # adds zerodb to VS Code's MCP config
zerodb verify --data-dir ./data                  # checks a stopped data dir on a copy
zerodb recover --truncate-at <lsn> --data-dir ./data
```

The config is JSON: `{"data_dir", "listen", "unix_socket", "unix_socket_group", "tenants": [{"name", "token"}]}`.
Flags override fields. It must be a regular file owned by you with no group or other bits.
Tenant names are 1 to 255 bytes. Tokens are 32 to 1024 bytes of visible ASCII, so the `Authorization` header fits the 16 KiB request head. Duplicate names or tokens fail.

## `zerodb serve`

- TCP listens on `127.0.0.1:7425` by default. `--listen <addr>` picks another loopback address, port 0 included. `--listen none` binds no TCP. A non-loopback address fails startup, because zerodb has no TLS. Put a TLS proxy on loopback in front.
- The Unix socket defaults to `<data_dir>/zerodb.sock`. Set it with `--unix-socket`. Its parent must be owned by the server's user and not writable by group or others. The socket is mode 0600. `--unix-socket-group <gid or name>` makes it 0660 with that group. Names come from `/etc/group`. Groups from LDAP or other NSS sources need the numeric gid.
- For sandboxes, put the socket in its own directory. See [sandboxes.md](../../docs/deploy/sandboxes.md).
- The data directory is created with mode 0700. An existing one must be owned by the server's user with no group or other bits, or startup fails.
- Startup order: config, TCP, then the engine and its lock, then the Unix socket. Only the lock holder replaces a stale socket. A path with a live server or a non-socket file fails startup. A socket is stale only when a connect is refused. On Linux a live server whose accept queue is full makes the probe time out, so it counts as live. macOS refuses that connect the same way it refuses a stale socket, so there a second server on the same socket path can replace a saturated live server's socket. Give each server its own socket path.
- The socket is bound in a per-start `<parent>/.zs.<8 hex>` directory, then hard-linked into place under a `flock` on `<socket>.lock`. Servers on other data directories that share the path take turns.
- A crash during that step can leave a `.zs.*` directory behind. The `.lock` file always stays. Both are harmless. A killed server also leaves its socket file. The next start on that path removes it.
- `LOCK` and `GET /healthz` carry a per-start instance id.
- At most 256 connections. Past that, a new connection gets 503 `busy`. A request body grows only as its bytes arrive, up to 8 MiB, and is dropped once parsed. At most 2 JSON trees exist at once, request parses and window builds together, and a tree can be about 16x its body. Window bodies being written total at most 256 MiB. A window that waits 5 s for that budget gets 503 `busy`. Memory stays under about 256 x 8 MiB = 2 GiB of raw bodies or turns, plus 2 trees of about 16 x 8 MiB, plus 256 MiB + 2 x 49 MiB of window bodies, in the worst case.
- `--exit-on-poison`: when the engine is poisoned, stop accepting, finish responses in flight, remove the socket path if it is still the one this server installed, and exit 70. A fresh start recovers. Without it, a poisoned server serves reads and 503 `poisoned` until you restart it.
- Logs: startup, shutdown and 5xx answers on stderr, one line each with method, path, status, tenant and latency. `--log-requests` adds a line for every request. Never headers, bodies or query strings.
- Stop it with a signal. Every acknowledged write is already synced, so `kill` is safe.

| Exit code | Meaning |
|---|---|
| 1 | Startup failed: config, listener or socket checks |
| 2 | Usage error |
| 70 | Poisoned, with `--exit-on-poison` |
| 75 | Another process holds the data directory lock |

## `zerodb verify` and `zerodb dump`

Both check a stopped data directory without changing it. Opening a data directory repairs it, so they never open the original. They take its `LOCK` without creating anything, copy its regular files into a new mode 0700 directory, open the copy, and remove it, also on failure. The lock stops a server from starting during the copy.

- `zerodb verify --data-dir <dir>` runs the same engine open as `zerodb serve`. It prints the `GET /v1/stats` fields except `instance` as sorted `key=value` lines. It exits 1 when the open fails or a checkpoint was rejected. A failed open names the first bad LSN. `zerodb recover --truncate-at` needs a transaction boundary below it; an LSN inside a transaction fails and names the nearest boundaries.
- `zerodb dump --data-dir <dir> [--from <lsn>] [--limit <n>]` prints one line per durable record from `--from`, default 1: `lsn`, `txn`, `kind`, `tenant`, payload `bytes` and `commit_time_ns`. It never prints content.
- The copy is `<dir>.verify-<8 hex>`, next to the data directory. `--scratch <dir>` puts it in that directory instead. It needs as much free space as the data directory, unless the filesystem clones files, as APFS, btrfs and XFS can.

Exit 0 on success, 1 when the open fails or the directory has no `LOCK`, 2 on bad flags or a `--scratch` inside the data directory, 75 when a server holds the lock. Copies are mode 0600, so a read-only backup opens too.

## HTTP API

Send `Authorization: Bearer <token>`. The token picks the tenant. On TCP the `Host` header must be `127.0.0.1`, `localhost` or `[::1]`, with an optional port. On the Unix socket `Host` is not checked.

| Route | Request | 200 response |
|---|---|---|
| `POST /v1/append` | `{"agent", "session", "idem_key", "turns": [{"role", "content", "meta"?}]}` | `{"outcome": "committed", "deduplicated", "txn", "first_lsn", "last_lsn", "seqs"}` |
| `GET /v1/window` | `?agent=&session=` plus `last=N`, or `since_seq=S` with an optional `limit=N`. Optional `max_bytes=B` | `{"turns": [{"seq", "lsn", "commit_time_ns", "role", "content", "meta"}], "truncated"}` |
| `GET /v1/search` | `?q=` words, plus optional `limit=N` (default 10, cap 100), `before=LSN` and `max_bytes=B` | `{"hits": [{"kind": "turn", "agent", "session", "seq", "lsn", "commit_time_ns", "role", "content", "meta"} or {"kind": "fact", "key", "lsn", "commit_time_ns", "agent", "content", "meta"}], "next"}` |
| `GET /healthz` | No token | `{"status": "ok" or "read_only", "instance"}`. 503 with `"poisoned"` |
| `GET /v1/stats` | Unix socket only, no token | One flat object: `instance`, `health`, `durable_lsn`, tenant and fact counts, writer groups, checkpoint and lock timings, tape-index memory. Counters cover this open |
| `GET /metrics` | Unix socket only, no token | The same numbers in Prometheus text format 0.0.4: counters as `zerodb_<name>_total`, gauges as `zerodb_<name>`, `_ns` maxima as `_seconds`, `zerodb_read_only`, the `zerodb_append_ack_seconds` histogram of committed appends (50 µs to 1 s), and the `zerodb_turn_content_bytes` histogram of new turns (64 B to 4 MiB). `turn_repeats` counts new turns whose content matches one of about 4,096 recent hashes, a lower bound on exact repeats |

`/v1/stats` and `/metrics` count activity across every tenant, so no tenant token can read them. Whoever can open the socket can, which the socket's mode and `--unix-socket-group` decide. On TCP they are 404 behind the token.
`zerodb load --url <unix or http url> --token-file <path> --writers W --turns N [--batch B] [--size S] [--think MS]` measures the server the way agents write. Each writer keeps one connection and waits for each ack before the next append, to its own session. It prints turns/s and p50, p99, p99.9 and max ack latency as `key=value` lines. `--think` pauses each writer a random 0 to 2x that many ms before every append, like an agent's model call. `--think` is at most 4000, so the first pause stays under the 10 s a new connection gets to send its first request. Defaults: 1 writer, 10,000 turns, batch 1, 256-byte turns, no think. The token can also come from `ZERODB_TOKEN`. `--turns` must divide by `--writers * --batch`. Exit 0, 1 on a failed append, 2 on bad flags.
`zerodb stats --data-dir <dir>` or `--url unix:///path` prints it as sorted `key=value` lines for grep. It connects once and never starts a server. Exit 0, 1 when no server answers, 2 on bad flags.

- `role` is `system`, `user`, `assistant` or `tool`. `meta` is a JSON object, at most 64 KiB. `idem_key` is required, 1 to 128 bytes.
- `last` and `limit` are capped at 1,000. `max_bytes` defaults to 1 MiB and is capped at 8 MiB. Turns are never cut. `truncated` says the budget, not the query, ended the window.
- Search walks records newest first. A turn matches when its content holds every word of `q`, ignoring case, as a substring. A plural word also matches its singular (Harman's S-stemmer), so `keys` finds `key`. A fact matches on its key or content, and only its current value counts: a superseded value never shows. A fact changed while a call runs may be missing from that call, like any record written after it started. A call visits at most 1,000,000 records. Pass `next` as `before` to go on. `next` is null once the walk reaches the oldest record.
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
Tools: `memory_append {content, role?, session?, meta?}`, `memory_recent {n?, session?, max_bytes?}`, the fact tools, and `memory_search {query, limit?, before?, max_bytes?}`.
Read tools carry `readOnlyHint: true`, so VS Code runs them without asking. `initialize` returns short `instructions` on when to call each tool. Copilot CLI adds a server's instructions to the system prompt only for allowlisted servers, or with `--allow-all-mcp-server-instructions`.
Integers beyond ±(2^53 - 1), such as `commit_time_ns`, come back as strings in tool results, because JavaScript clients can't hold them (I-JSON, RFC 7493). The shell verbs print them as numbers.
`--agent` defaults to `mcp` and `--session` to `default`. A tool may pass `session`. The tenant still comes from the token.
The token comes from `--token-file`, `ZERODB_TOKEN`, or the config entry for `--tenant`. A token file must be 0600 and yours.

With none of `--url`, `--data-dir`, `--config`, `--token-file`, `--tenant` or `ZERODB_TOKEN`, it uses a per-user home.
On macOS the home is `~/Library/Application Support/zerodb`. On Linux it is `$XDG_DATA_HOME/zerodb`, else `~/.local/share/zerodb`.
The first use creates it with mode 0700, `zerodb.json` with tenant `me` at 0600, and `data/` at 0700.
The home and `data/` must be yours, with no group or other bits. The shell verbs use the same home.
Each use syncs the directory entries of `data/`, the home and its parents up to `$HOME`.
A client needs only this:

```json
{"mcpServers": {"zerodb": {"command": "zerodb", "args": ["mcp"]}}}
```

How it finds a server:

1. `--url unix:///path` or `--url http://127.0.0.1:<port>`. An operator chose it, so there is no instance check. `http` takes only `127.0.0.1`, `localhost` or `[::1]`, because the token travels in cleartext.
2. Else it reads `<data_dir>/LOCK` and uses the Unix socket named there. Never the TCP address. The socket must be yours, its parent owned by you and not writable by group or others. Every directory above it must be owned by root or you and not writable by group or others unless sticky, as OpenSSH's StrictModes checks. Then `GET /healthz` must return the instance id in `LOCK`. Only then does the token go out, on that same connection.
3. Else, with `--config` or the home, it starts `zerodb serve --config <path> --data-dir <dir> --listen none --exit-on-poison`. Output goes to `<data_dir>/serve.log`. The token is never on its command line.
4. It repeats step 2 until a server is trusted or 2 s before the call's deadline. A started server that exits 75 lost the lock race. If no trusted server shows up, it starts another, backing off from 100 ms to 2 s. A start that can't work fails the call at once with its reason: a data directory it can't use, or a started server that exits with any code but 75 or 70 (poisoned). The last line of `serve.log` is in the error. The next call tries again.

An auto-started server runs in its own process group and outlives the adapters. Stop it with `kill <pid>`. The pid is in `<data_dir>/LOCK`.

## `zerodb connect` and `zerodb disconnect`

`zerodb connect <vscode|copilot>` creates the home, then adds a `zerodb` entry to the harness's user MCP config.
`zerodb disconnect <vscode|copilot>` removes that entry. Every other key and server stays.

| Name | File | Entry |
|---|---|---|
| `vscode` | macOS: `~/Library/Application Support/Code/User/mcp.json`. Linux: `$XDG_CONFIG_HOME/Code/User/mcp.json`, else `~/.config/Code/User/mcp.json` | `servers.zerodb`: `zerodb mcp --agent vscode-copilot --session ${workspaceFolderBasename}` |
| `copilot` | `$COPILOT_HOME/mcp-config.json`, else `~/.copilot/mcp-config.json` | `mcpServers.zerodb`: `zerodb mcp --agent copilot`, all tools |

The entry runs the absolute path of the binary you ran. It holds no token and no config path.
A missing file is created with its directories. An entry that already matches writes nothing.
Before a change, the old file is copied to `<file>.zerodb-backup-<unix seconds>`. Backups are never restored automatically.
The new file goes to a synced temp file, then a rename. It keeps the old file's mode.
The rewrite sorts the file's keys and drops its formatting.
A file that is not plain JSON, such as one with `//` comments, is left alone. The error shows the entry to add by hand.
A symlinked file is refused, because the rename would replace the link.

## Tests

Socket paths, staging included, must fit `sun_path` (104 bytes on macOS). Tests put them under `target/s`. Set `ZERODB_TEST_DIR` to a short directory if your checkout path is too deep.
