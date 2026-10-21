# zerodb

The server binary. HTTP/JSON over loopback TCP and a Unix socket, with the same token auth on both.

```sh
zerodb init --config zerodb.json --tenant me     # writes a 0600 config, prints the token once
zerodb serve --config zerodb.json --data-dir ./data
zerodb recover --truncate-at <lsn> --data-dir ./data
```

## `zerodb serve`

- TCP listens on `127.0.0.1:7425` by default. `--listen <addr>` picks another loopback address, port 0 included. `--listen none` binds no TCP. A non-loopback address fails startup, because zerodb has no TLS. Put a TLS proxy on loopback in front.
- The Unix socket defaults to `<data_dir>/zerodb.sock`. Set it with `--unix-socket`. Its parent must be owned by the server's user and not writable by group or others. The socket is mode 0600. `--unix-socket-group <gid or name>` makes it 0660 with that group. Names come from `/etc/group`. Groups from LDAP or other NSS sources need the numeric gid.
- The data directory is created with mode 0700. An existing one must be owned by the server's user with no group or other bits, or startup fails.
- The socket is bound in a per-start `.zs.<8 hex>` directory next to it, then linked into place under a `flock` on `<socket>.lock`. A crash during that step can leave one such directory behind. It is safe to delete. The `.lock` file stays. It is harmless.
- Startup order: config, TCP, then the engine and its lock, then the Unix socket. Only the lock holder replaces a stale socket. A path with a live server or a non-socket file fails startup.
- `LOCK` and `GET /healthz` carry a per-start instance id.
- At most 256 connections. Past that, a new connection gets 503 `busy`. A request body grows only as its bytes arrive, up to 8 MiB, and is dropped once parsed. Body memory stays under 256 x 2 x 8 MiB = 4 GiB in the worst case.
- `--exit-on-poison`: when the engine is poisoned, stop accepting, finish responses in flight, remove the socket path if it is still the one this server installed, and exit. A fresh start recovers.
- Logs: one stderr line per request with method, path, status, tenant and latency. Never headers, bodies or query strings.

| Exit code | Meaning |
|---|---|
| 1 | Startup failed: config, listener or socket checks |
| 2 | Usage error |
| 70 | Poisoned, with `--exit-on-poison` |
| 75 | Another process holds the data directory lock |

## Tests

Socket paths, staging included, must fit `sun_path` (104 bytes on macOS). Tests put them under `target/s`. Set `ZERODB_TEST_DIR` to a short directory if your checkout path is too deep.
