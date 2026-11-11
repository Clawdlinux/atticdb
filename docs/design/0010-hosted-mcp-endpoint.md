# 0010: A hosted MCP endpoint

Status: accepted, built on branch feat/hosted-mcp, 2026-11-11. Proposed 2026-11-10. This is Phase 1 of [0009](0009-default-memory-layer.md). The owner picked approach B below.

## Why

The first user is a developer whose agents run on several machines and in the cloud. Today each machine has its own store. A cloud agent with no `atticdb` binary can't reach any store. We want every agent to read and write 1 hosted tenant by adding a URL and a token. Nothing gets installed.

## How agents connect today

- `atticdb mcp` speaks MCP over stdio. Each tool call goes through `Client` to `atticdb serve` over a Unix socket or loopback TCP.
- `serve` only binds loopback TCP. It refuses a request whose `Host` isn't loopback (`api::check`).
- Tenants and their tokens live in `atticdb.json`. A token picks its tenant.

## Approaches

| | A: as 0008 says | B: hosted MCP endpoint, token | C: B plus OAuth |
|---|---|---|---|
| How an agent connects | local `atticdb mcp` → https → hosted `serve` | `{"type": "http", "url": "https://<host>/mcp", "headers": {"Authorization": "Bearer <token>"}}` | same URL, and Claude asks you to sign in |
| Needs a local install | yes | no | no |
| Copilot CLI, VS Code, Claude Code, Codex, Gemini CLI | yes | yes | yes |
| Claude Connectors (web, Desktop, mobile) | no | no | yes |
| Work | small | medium | large: an OAuth authorization server |

**Picked: B.**
- It serves the first user with no install, and it reuses the 9 tools and token tenants we have.
- C needs B's endpoint anyway. It also needs real accounts, which is a later step.
- Claude's Connectors screen needs OAuth or no auth. A bearer-token header there is in beta for a few organizations only ([Claude connector auth](https://claude.com/docs/connectors/building/authentication)).

## Goals

- `atticdb serve` answers MCP at `POST /mcp`, using Streamable HTTP ([MCP 2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)).
- It uses the same tools, outputs and conflict guard (D107) as `atticdb mcp`, with no copy of the tool code.
- It runs behind a TLS proxy on 1 VM, with a runbook to stand it up.

## Non-goals

- OAuth and sign-up (C).
- `atticdb connect --url` writing remote entries into each app. The README gets a snippet per app instead. That's the next step.
- Server-to-client messages: the SSE stream, `roots/list` and resumption.
- Scoped tokens (#35), per-tenant quotas and backup. Each gets its own issue (0008).

## Anti-goals

- A second tool implementation that can drift from `atticdb mcp`.
- Any way to reach the store without a token.
- A token in a URL or a log line.

## Design

### Endpoint

- **Routes:**
  - `POST /mcp` takes 1 JSON-RPC message per request, as the spec requires.
  - `GET /mcp` and `DELETE /mcp` return 405. There's no SSE stream, and no client-driven session end.
- **Auth:** the same `Authorization: Bearer <token>` check as every route. The token picks the tenant. A missing or bad token gets 401, as today.
- **`Origin`:** a request whose `Origin` is a web origin (`http://` or `https://`) gets 403, against DNS rebinding as the spec requires. See call 2 below.
- **Answers:**
  - A request gets `200` with `Content-Type: application/json` and 1 JSON-RPC response.
  - A notification or a response gets `202` with no body.
  - A body that isn't JSON-RPC gets `400`, with a JSON-RPC parse or invalid-request error.
- **`MCP-Protocol-Version`:** checked against the versions `mcp.rs` supports. An unknown value gets `400`. A missing header is allowed.

### Sessions

- `initialize` creates a session. The reply carries `MCP-Session-Id`, 32 random hex bytes from `crate::random_hex`.
- **The session holds what the stdio adapter keeps per process:** whether it's initialized, the agent name, the default session name, and the `seen` map that makes `fact_set` conditional (D107).
- **Any other message must carry `MCP-Session-Id`:**
  - Without it: `400`.
  - With an id that's unknown, expired, or made under another tenant's token: `404`, so the client starts a new session.
- **Limits:** at most 64 sessions per tenant and 4,096 in all. A session idle for 1 hour is dropped. When a cap is hit, the least recently used session goes. All of this is in memory only. A restart drops every session, and clients re-initialize after the 404.

### Shared code, not a copy

- `Client` gets a second transport.
  - Today `Client::request` sends HTTP to a server.
  - The new transport calls the same `api` handler `serve` would call for that route (`api::append`, `api::fact_read` and the rest). It runs in-process, with the session's tenant, and parses the handler's JSON body the same way.
  - Everything above `request` runs unchanged: retries, outcome merging, redaction, rendering and the conflict guard.
- The JSON-RPC handling in `mcp.rs` (`Adapter::handle`) gets split.
  - The part that turns 1 message into at most 1 reply is shared.
  - The stdio loop and the `roots/list` exchange stay in the stdio adapter only.
- **Tools:** `file_ref` and `file_get` read files on the machine running them. On a hosted server that's the wrong machine. So `/mcp` lists 7 tools and answers those 2 with "unknown tool".

### Default session name

- Over stdio, the session name comes from the client's roots: the repo folder.
- `/mcp` doesn't ask for roots, so the default is `default`.
- The `initialize` instructions on `/mcp` add 1 sentence: pass `session` set to the repo's main folder name on `memory_append` and `memory_recent`.
- They drop the `file_ref` sentence and the 2 on `atticdb forget` and the CLI fallback. Those act on a local store, not the hosted one.
- The stdio instructions don't change.

### The chat-start block

- Hooks run a local binary, so remote agents get no block.
- The instructions already say: "If no block arrived, call fact_list with prefix user/ and with repo/<repo>/." No change.

### Hosting

- **`serve` keeps binding loopback only** (`listen.rs`).
- **Caddy runs on the same VM.** It gets a certificate for `<host>`, terminates TLS and proxies to `127.0.0.1:<port>`. It sets `Host` to `127.0.0.1`, so the loopback `Host` check passes unchanged.
- **The VM:** 1 Azure Linux VM, with the data dir on a Premium SSD v2 managed disk, as in 0008. Never the local NVMe: it's wiped on deallocate.
- **Tenants:** for the first users, each tenant is a config entry made by hand with a random token. That's `atticdb init`, or an edit to `atticdb.json` and then a restart.
- **The runbook** in `docs/hosting.md` covers:
  - the VM, disk and DNS
  - Caddy
  - `serve` as a systemd service
  - adding a tenant
  - rotating a token
- **Spend:** provisioning the VM needs the owner's go-ahead and a spend cap. Building and testing the endpoint needs neither.

### Client config snippets (README)

1 snippet per app, all with the same URL and header:
- Copilot CLI (`~/.copilot/mcp-config.json`, `type: http`)
- VS Code (`mcp.json`, `type: http`)
- Claude Code (`claude mcp add --transport http atticdb <url> --header "Authorization: Bearer <token>"`)
- Codex (`config.toml` `url` plus `bearer_token_env_var`)
- Gemini CLI (`httpUrl` plus `headers`)

The builder checks each app's current docs before writing its snippet.

## Testing

Integration tests drive `serve` over loopback TCP with real HTTP:
- `initialize` returns `MCP-Session-Id`, the notification gets `202`, and `tools/list` lists 7 tools.
- `fact_set` then `fact_get` round-trip.
- 2 sessions on 1 tenant: the stale `fact_set` conflicts (D107), and the retry commits.
- A missing session id gives 400. An unknown id gives 404. Another tenant's token with a valid id gives 404.
- `Origin: https://evil.example` gives 403, and `Origin: vscode-file://vscode-app` is allowed. `GET` and `DELETE` give 405. A bad `MCP-Protocol-Version` gives 400. No token gives 401.
- The agent name comes from `clientInfo.name`, cleaned up as in call 1.
- `file_ref` gives "unknown tool".
- `atticdb mcp` over stdio passes its existing tests unchanged.

## Left to the builder

- How `Client` holds its transport: an enum or a small trait. Pick whichever matches the code around it.
- The session store's data structure, within the limits above.
- Exact log lines. They must not hold tokens or session ids.

## Calls made after the builder check

These are my calls, not the owner's. Each says what it costs if wrong.

1. **The agent name comes from `clientInfo.name` in `initialize`.**
   - Lowercase it, change every character outside `a-z`, `0-9` and `-` to `-`, and cut it to 64 bytes. If it's empty, use `mcp`.
   - The session stores it. Notes then show which app wrote them, and the conflict text names the writer.
   - In Phase 1 a token is 1 person's tenant, so trusting the name costs nothing. Scoped tokens (#35) can pin it later.
   - Cost if wrong: hosted names like `visual-studio-code` won't match local names like `vscode`. Hosted and local stores are separate in Phase 1, so nothing mixes.
2. **`Origin` check: refuse only web origins.**
   - A request whose `Origin` starts with `http://` or `https://` gets 403. That's what a DNS-rebinding page would send.
   - No `Origin`, `null` and other schemes (such as an editor's `vscode-file://`) are allowed. Every request still needs the bearer token, which a rebinding page doesn't have.
   - The runbook's first-user check confirms each of the 5 apps connects.
   - Cost if wrong: an app that sends an `https://` origin gets 403. Fix it with an allowlist then.
3. **The in-process transport tries once and never sleeps.**
   - Nothing is sent over a network, so there's no transport error to retry. The handler's answer is final: `committed`, `not_committed` or an error.
   - The 30 s retry deadline applies to the HTTP transport only.
   - Cost if wrong: none. A client that wants a retry sends the call again.
