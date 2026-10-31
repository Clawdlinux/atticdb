`atticdb mcp` gives the model `memory_append`, `memory_recent`, `fact_set`, `fact_get`, `fact_list` and `memory_search`. Search finds turns and current facts by words, across every session, newest first. It starts the server if none is running.

The model can read and write any session in the tenant. Don't give it to a model you don't trust with the whole tenant.

## Tools

`atticdb mcp` speaks newline-delimited JSON-RPC 2.0 on stdin and stdout. Logs go to stderr. It exits 0 when stdin closes.

- `memory_append {content, role?, session?, meta?}`
- `memory_recent {n?, session?, max_bytes?}`
- the fact tools: `fact_set`, `fact_get` and `fact_list`
- `memory_search {query, limit?, before?, max_bytes?}`

Read tools carry `readOnlyHint: true`, so VS Code runs them without asking. `initialize` returns short `instructions`: use the memory block, and where to save rules and notes. Claude Code and Codex read them. Copilot CLI adds a server's instructions to the system prompt only for allowlisted servers, or with `--allow-all-mcp-server-instructions`.

Integers beyond ±(2^53 - 1), such as `commit_time_ns`, come back as strings in tool results. JavaScript clients can't hold them (I-JSON, RFC 7493). The shell verbs print them as numbers.

## Secrets

Before `memory_append` and `fact_set` write, AtticDB replaces secrets with placeholders such as `[secret:github-token]`. That covers the content and every `meta` string. The shell verbs `append` and `fact set` do the same. The result's `redacted` field counts what was replaced, by type.

It looks for these:

- the values of env vars whose names end in `_KEY`, `_TOKEN`, `_SECRET` or `_PAT`, or contain `PASSWORD`, if 12 bytes or longer;
- GitHub tokens, AWS key ids, Slack tokens, Stripe live keys, Anthropic and OpenAI keys;
- private key blocks, `Bearer` tokens, and passwords in URLs.

It is best effort. A secret that was changed, split or encoded gets through, so don't save secrets.

## The home

A client needs only this:

```json
{"mcpServers": {"atticdb": {"command": "atticdb", "args": ["mcp"]}}}
```

With no connection flags, `atticdb mcp` and the shell verbs use a per-user home, created on first use. On macOS it is `~/Library/Application Support/atticdb`. On Linux it is `$XDG_DATA_HOME/atticdb`, else `~/.local/share/atticdb`.

`ATTICDB_HOME` moves it, for example to a test folder. It must be an absolute path. Empty counts as unset. It holds `atticdb.json` with tenant `me`, and `data/`. Any connection flag skips the home. Use absolute paths in flags.

With no connection flags, `ATTICDB_TOKEN` is ignored and a note says so. The home's token is used. The first use creates the home with mode 0700, `atticdb.json` at 0600, and `data/` at 0700. The home and `data/` must be yours, with no group or other bits.

## Sessions and agents

`--agent` defaults to `mcp`, as in the shell verbs. The session-start hook reads that agent's notes too. A tool may pass `session`. An empty `session` counts as none, so the call uses the default session. The tenant still comes from the token.

`--session` defaults to a repo name. When the client reports roots, as VS Code does, it is the repo of the first `file://` root. Without roots, it is the repo of the current directory. The shell verbs use that too.

The repo is the name of the first directory up with a `.git` entry, else of that directory. A worktree takes its main repo's name, so both share memory. 2 repos with the same folder name share a session and `repo/<name>/` facts.

The token comes from `--token-file`, `ATTICDB_TOKEN`, or the config entry for `--tenant`. A token file must be 0600 and yours.

## How it finds a server

1. `--url unix:///path` or `--url http://127.0.0.1:<port>`. An operator chose it, so there is no instance check. `http` takes only `127.0.0.1`, `localhost` or `[::1]`, because the token travels in cleartext.
2. Else it reads `<data_dir>/LOCK` and uses the Unix socket named there. Never the TCP address. The socket and the directories above it must pass owner and mode checks. Then `GET /healthz` must return the instance id in `LOCK`. Only then does the token go out, on that same connection.
3. Else, with `--config` or the home, it starts `atticdb serve --config <path> --data-dir <dir> --listen none --exit-on-poison`. Output goes to `<data_dir>/serve.log`. The token is never on its command line.
4. It repeats step 2 until a server is trusted or 2 s before the call's deadline. If no trusted server shows up, it starts another, backing off from 100 ms to 2 s.

An auto-started server runs in its own process group and outlives the adapters. Stop it with `kill <pid>`. The pid is in `<data_dir>/LOCK`.

Writes follow [the outcome rule](docs:http-api#the-outcome-rule). Retries run until the retry deadline, 30 s by default. Set it with `--retry-deadline`.

Source: [README.md](https://github.com/Clawdlinux/atticdb/blob/main/README.md) and [crates/atticdb/README.md](https://github.com/Clawdlinux/atticdb/blob/main/crates/atticdb/README.md)
