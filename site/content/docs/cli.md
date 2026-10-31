The same 6 tools run from a shell, with `atticdb mcp`'s connection flags. Each prints 1 JSON line.

## Shell verbs

```sh
export ATTICDB_TOKEN=$TOKEN
Z="--url unix://$PWD/data/atticdb.sock --agent planner --session run-1"
atticdb append $Z --content "hi" --role user          # {"committed":true,"deduplicated":false,"lsn":3,"seq":2}
atticdb recent $Z --n 10                              # {"truncated":false,"turns":[...]}
atticdb fact set $Z --key repo/payments/retry-rule --content "Retry 3 times, then fail."
atticdb fact get $Z --key repo/payments/retry-rule    # {"fact":{"agent":"planner","content":...}}
atticdb fact list $Z --prefix repo/ --limit 50        # {"facts":[...],"next":null}
atticdb search $Z --query "retry rule"                # {"hits":[...],"next":null}
```

`fact set --delete` deletes. With no connection flags, the shell verbs use the same per-user home as [`atticdb mcp`](docs:mcp#the-home).

Each write picks 1 random `idem_key`. It retries with it until `--retry-deadline`, 30 s by default.

| Exit code | Meaning |
|---|---|
| 0 | Done |
| 1 | Nothing was written, or a read failed |
| 2 | A usage error |
| 3 | The write may have landed. Running the command again uses a new key, so it can write a duplicate |

`append` and `fact set` hide secrets before they write, as the [MCP tools](docs:mcp#secrets) do.

## Studio

`atticdb studio` serves a read-only web page about your memory, on loopback, and opens it in your browser.

```sh
atticdb studio             # the home, opens your browser
atticdb studio --no-open   # only prints the link
```

It takes `atticdb mcp`'s connection flags: `--url`, `--data-dir`, `--config`, `--tenant` and `--token-file`. With none, it uses the home and starts its server if needed. Each run listens on `127.0.0.1` on a new random port. It prints a link with a secret of 32 hex characters:

```text
atticdb studio: http://127.0.0.1:<port>/#k=<secret>
Press Ctrl-C to stop.
```

Unless `--no-open`, it writes a redirect page with that link to `studio.html` in the home directory, at mode 0600. Then it runs `open` on macOS or `xdg-open` on Linux on that file. So the secret is never in a process argument, where `ps` shows it to other users.

Snap browsers, the default on Ubuntu, cannot read hidden folders such as `~/.local/share`. They show an error. Then open the printed link by hand, or pass `--no-open`.

The page has 7 screens: Overview, Log, Sessions, Facts, Search, Chat start and Health. Each is at a hash route.

To try it on realistic memory, run `crates/atticdb/studio/seed.sh <empty dir>`. Then run `HOME=<dir> XDG_DATA_HOME= ATTICDB_HOME= atticdb studio`.

## How the studio stays private

The page reads the secret from the link's fragment, which a browser never sends to a server. It sends the secret back in the `X-Atticdb-Studio` header. The tenant token stays in the studio, and no response carries it.

Every request goes through these checks, in order:

- `Host` must be exactly `127.0.0.1:<port>`, else 403 `forbidden`. This blocks DNS rebinding.
- An `Origin` header, when sent, must be `http://127.0.0.1:<port>`, else 403 `forbidden`.
- Only `GET`, else 405 `method_not_allowed`. Request bodies are refused.
- Routes under `/api/` need the secret, else 401 `unauthorized`. It is compared in constant time.

| Route | Answer |
|---|---|
| `GET /`, `/app.js`, `/app.css` | The page. No secret needed. It holds no data |
| `GET /api/v1/sessions`, `records`, `window`, `search`, `facts/list`, `facts/get`, `facts/history` | The store's `GET /v1/...` with the same query, sent with the token. Its status and body come back unchanged |
| `GET /api/v1/stats`, `GET /api/metrics` | The store's `/v1/stats` and `/metrics`. A TCP store refuses them, and that answer comes back |
| `GET /api/block?repo=<name>` | `{"repo", "block": <text> or null, "chars", "max": 6000}`: the session-start hook's block for that repo, built by the hook's code. Only on the home, else 404 `not_home`. `repo` is 1 to 255 bytes and the only parameter, else 400 `invalid_input`. A failed read is 502 `store_unreachable` |
| `GET /api/info` | `{"version", "tenant", "store": "home", "data_dir" or "url", "home": <path> or null}`. `tenant` is null when the token came from a token file or `ATTICDB_TOKEN` |

Anything else is 404 `not_found`. No route writes to the store. A store read has 10 s, connect included. Past that it answers 504 `store_timeout`. At most 16 connections are open. Past that, a new one gets 503 `busy`.

Exit code 1 when it cannot start, 2 for a bad flag.

Source: [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md) and [crates/atticdb/README.md](https://github.com/Clawdlinux/atticdb/blob/main/crates/atticdb/README.md)
