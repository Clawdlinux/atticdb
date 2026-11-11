# Hosted MCP endpoint (Phase 1 of 0009). Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any agent reads and writes 1 hosted tenant over `POST /mcp` with a URL and a bearer token. Nothing gets installed.

**Architecture:**
- `Client` gets an in-process transport that calls the same `api` handlers `serve` routes to.
- `mcp.rs` gets a hosted session type, `Hosted`, built on the same JSON-RPC handler as stdio.
- A new `hosted.rs` adds the HTTP endpoint and its session store.
- `serve` routes `POST /mcp` to it.

**Tech Stack:** Rust (crates/atticdb). No new dependencies.

**Spec:** `docs/design/0010-hosted-mcp-endpoint.md`, including "Calls made after the builder check".

## Global Constraints

- No new dependencies.
- 1 tool implementation. `/mcp` and `atticdb mcp` both go through `Client::call`. No tool logic is copied.
- `atticdb mcp` over stdio behaves exactly as today. Its existing tests pass unchanged.
- The model never sees `if_lsn`, tokens or tenants (D76). Tokens and session ids never appear in a URL, a log line or an error message.
- `serve` still binds loopback only. The loopback `Host` check stays.
- Session limits: 64 per tenant, 4,096 in all, 1 hour idle. Least recently used goes first. Memory only.
- `/mcp` lists 7 tools. `file_ref` and `file_get` answer JSON-RPC `-32602` "unknown tool".
- The agent comes from `clientInfo.name`:
  - lowercase it
  - change every character outside `a-z`, `0-9` and `-` to `-`
  - cut it to 64 bytes
  - if empty or missing, use `mcp`
- `Origin` starting with `http://` or `https://` gives 403. Any other `Origin`, or none, is allowed.
- In-process calls try once and never sleep.
- No Azure resource is created in this plan.
- Prose: no em dashes, sentences under 20 words, numbers as digits. Commits: `git commit -s`, no Co-authored-by.

## Review Focus

1. **2 requests at once on 1 session** run one after the other on that session's lock, with no panic and no lost `seen` entry. Test in Task 2.
2. **A JSON array body (a batch)** gets 400 with JSON-RPC `-32600`. Test in Task 2.
3. **`notifications/initialized` without a session id** gets 400, and is not treated as a new session. Test in Task 2.
4. **A `clientInfo.name` that is 500 bytes, all symbols, or not a string** gives a valid agent name. Unit test in Task 1.
5. **A session id from tenant A used with tenant B's token** gives 404, and tenant B's store is untouched. Test in Task 2.

---

### Task 1: In-process transport and the `Hosted` MCP session

**Files:**
- Modify: `crates/atticdb/src/api.rs`
  - add `tenant_route` and `dispatch`
  - `serve.rs` calls `dispatch` for the 12 tenant routes, `Append` to `Export`, in its `match route` (serve.rs ~159-185)
- Modify: `crates/atticdb/src/serve.rs` (the match above only)
- Modify: `crates/atticdb/src/client.rs`
  - the `Client` struct (~43) and `open` (~388)
  - `request` (~853) and `retry` (~830)
- Modify: `crates/atticdb/src/mcp.rs`
  - `Adapter` (~26), `handle` (~123) and `tools` (~260)
  - `INSTRUCTIONS` stays byte-identical
- Test: unit tests in `mcp.rs`, and `crates/atticdb/tests/it/mcp.rs`

**Interfaces:**
- Produces in `api.rs`:
  - `pub fn tenant_route(method: &str, path: &str) -> Option<Route>`, the `ROUTES` entry whose path and method both match
  - `pub fn dispatch(engine: &Engine, tenant: &str, route: Route, target: &str, body: <the type of request.body today>) -> Response`. It's the existing handler call for that route. It's unreachable for `Health`, `Stats` and `Metrics`.
- Produces in `client.rs`:
  - `pub(crate) fn Client::local(engine: Arc<Engine>, tenant: String) -> Client`. Agent `mcp`, session `default`, `seen: None`.
  - `pub(crate) fn Client::set_agent(&mut self, agent: String)`
- Produces in `mcp.rs`:
  - `pub(crate) const VERSIONS` (was private)
  - `pub fn agent_name(client_info_name: Option<&str>) -> String`, per the Global Constraints rule
  - `pub struct Hosted`
  - `pub fn Hosted::new(engine: Arc<Engine>, tenant: &str) -> Hosted`, which calls `guard_facts`
  - `pub fn Hosted::handle(&mut self, message: &[u8]) -> Option<Value>`, with the same contract as `Adapter::handle`: at most 1 reply, and none for notifications and responses

- [ ] **Step 1: Write the failing tests.**
  - Unit tests in `mcp.rs` for `agent_name`:
    - `Some("Visual Studio Code")` gives `visual-studio-code`
    - `Some("claude-code")` gives `claude-code`
    - `Some("")` gives `mcp`, and so does `None`
    - `Some("!!!")` gives `---`
    - `Some(&"x".repeat(500))` gives 64 `x`
    - non-ASCII `Some("Ä")` gives `-`
  - `tests/it/mcp.rs`, new `hosted_session_runs_the_same_tools`. Use `support::sim_engine` and `atticdb::mcp::Hosted::new(engine.clone(), "a")`. Send `initialize` with `clientInfo: {"name": "Visual Studio Code"}`.
    - The reply has `serverInfo.name == "atticdb"` and `capabilities.roots` absent.
    - Its `instructions` contain `On this hosted store, pass session set to the repo's main folder name on memory_append and memory_recent.`
    - Its `instructions` do not contain `Leave out the session argument.`
    - After `notifications/initialized`, which gives `None`, `tools/list` names exactly:
      `memory_append, memory_recent, fact_set, fact_get, fact_list, memory_search, fact_history`
    - `tools/call file_ref` gives error code `-32602`.
    - `fact_set` `repo/x` = `one` commits, and `fact_get` shows agent `visual-studio-code`.
    - `memory_append` without `session` writes to session `default`. With `session: "atticdb"` it's readable by `memory_recent` with `session: "atticdb"`.
  - New `hosted_sessions_guard_fact_writes`. 2 `Hosted` on tenant `a`:
    - h1 sets `repo/k` to `one`
    - h2 sets it to `two`, and gets an error starting with `conflict:`
    - h2 sets `two` again, and it commits
- [ ] **Step 2: Run, expect FAIL (no `Hosted`):** `timeout 900 cargo test -p atticdb --test it mcp::hosted_ && timeout 600 cargo test -p atticdb --lib mcp::`
- [ ] **Step 3: Implement.**
  - `api::dispatch`: move the existing tenant-route calls out of `serve.rs` without changing them. `serve.rs` keeps `Health`, `Stats` and `Metrics`.
  - `Client`: hold the transport in 1 enum.
    - The HTTP variant holds `token`, `launch` and `retry_deadline`.
    - The local variant holds `engine` and `tenant`.
    - In the local variant, `request` finds the route with `tenant_route`, calls `dispatch` and parses the body as today. An unrouted target is `Attempt::NotSent`.
    - `retry` with the local variant judges once and returns `Ok` or `Err` alike, with no sleep.
    - `found()` returns `None` for the local variant.
  - `Adapter`: add `hosted: bool`. When hosted:
    - roots are never asked for
    - `initialize` sets the agent from `params.clientInfo.name` through `agent_name`
    - the instructions replace `Leave out the session argument.` with the hosted sentence
    - `tools/list` and `tools/call` drop `file_ref` and `file_get`
  - `Hosted` wraps a hosted `Adapter` built on `Client::local`.
  - Add 1 assert to `instructions_stay_within_word_budget`: the hosted variant is also at most 270 words.
- [ ] **Step 4: Run, expect PASS:**
  - Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && timeout 1500 cargo test --workspace`
  - The known flaky test (#201) may fail once. Rerun it alone.
- [ ] **Step 5: Commit:** `git commit -s -m "feat(mcp): in-process transport and a hosted MCP session"`

### Task 2: `POST /mcp` in `serve`

**Files:**
- Create: `crates/atticdb/src/hosted.rs` for the endpoint and session store. Add `pub mod hosted;` in `lib.rs`.
- Modify: `crates/atticdb/src/http.rs`. `Response.headers` becomes `Vec<(&'static str, String)>`. Update every constructor, including `export.rs:74` and `studio.rs` `HEADERS`.
- Modify: `crates/atticdb/src/api.rs`. Add `Route::Mcp` and the `("POST", "/mcp", Route::Mcp)` entry. `GET` and `DELETE` then get the existing 405.
- Modify: `crates/atticdb/src/serve.rs`. `Shared` holds `sessions: hosted::Sessions<crate::mcp::Hosted>`. `Route::Mcp` calls `hosted::mcp`.
- Test: unit tests in `hosted.rs`, and a new `crates/atticdb/tests/it/hosted.rs` registered in `tests/it/main.rs`.

**Interfaces:**
- Consumes: Task 1's `Hosted::{new, handle}`, `api::Route` and `mcp::VERSIONS`.
- Produces:
  - `pub struct Sessions<T>`
  - `Sessions::new(per_tenant: usize, total: usize, idle: Duration)`
  - `fn insert(&self, tenant: &str, value: T, now: Instant) -> String`. It returns a new id of 32 random hex bytes from `crate::random_hex`, and evicts the least recently used session when a cap is hit.
  - `fn get(&self, id: &str, tenant: &str, now: Instant) -> Option<Arc<Mutex<T>>>`. It returns `None` for an unknown id, another tenant's id, or an id idle past `idle`. A hit refreshes last use.
  - Constants `MAX_SESSIONS_PER_TENANT = 64`, `MAX_SESSIONS = 4096` and `SESSION_IDLE = 3600 s`.
  - `pub fn mcp(sessions: &Sessions<Hosted>, engine: &Arc<Engine>, tenant: &str, head: &Head, body: &[u8]) -> Response`

- [ ] **Step 1: Write the failing tests.**
  - Unit tests in `hosted.rs` on `Sessions<u32>`:
    - the 65th insert for 1 tenant evicts that tenant's least recently used session
    - `total` caps across tenants
    - `get` with another tenant gives `None`
    - `get` at `now + idle + 1 s` gives `None`
    - a `get` refreshes last use
  - `tests/it/hosted.rs`: run `support::serve` over TCP with tenants `a` and `b`, and send raw HTTP POSTs with `Host: 127.0.0.1`.
    - `initialize` gives 200 with `Content-Type: application/json` and an `mcp-session-id` header of 64 hex chars.
    - `notifications/initialized` with the id gives 202 and an empty body.
    - `tools/list` lists 7 tools.
    - `fact_set` then `fact_get` round-trips through 2 POSTs.
    - **Errors:**
      - no id on `tools/list` gives 400
      - an unknown id gives 404
      - tenant `a`'s id with `b`'s token gives 404
      - `notifications/initialized` with no id gives 400
    - **Origin:** `https://evil.example` gives 403 and runs nothing. `vscode-file://vscode-app` is allowed.
    - **Methods and versions:**
      - `GET /mcp` and `DELETE /mcp` give 405
      - `mcp-protocol-version: 1999-01-01` gives 400, and a supported value or none is allowed
      - no token gives 401
    - **Bodies:** `[]` and `[{...}]` give 400 with `error.code == -32600`. `{` gives 400 with `-32700`.
    - **Concurrency:** 2 threads send 20 `fact_set` calls each on 1 session, each call to its own key. All 40 commit, and a third session's `fact_list` shows 40 keys.
    - `atticdb mcp` stdio tests stay untouched.
- [ ] **Step 2: Run, expect FAIL:** `timeout 900 cargo test -p atticdb --test it hosted:: && timeout 600 cargo test -p atticdb --lib hosted::`
- [ ] **Step 3: Implement** in this order, per the design's Endpoint and Sessions sections:
  1. Origin
  2. protocol version
  3. parse the body as 1 JSON object
  4. route on session id or `initialize`
  5. lock the session and call `handle`
  6. answer `None` with 202, or `Some` with 200 JSON
  
  A new session is stored only when its `initialize` reply has a `result`.
- [ ] **Step 4: Full checks, expect PASS.** Same commands as Task 1, Step 4.
- [ ] **Step 5: Commit:** `git commit -s -m "feat(serve): POST /mcp with sessions, for hosted agents"`

### Task 3: Runbook, client snippets and the decision row

**Files:**
- Create: `docs/hosting.md`
- Modify: `README.md` and `crates/atticdb/README.md` with a "Hosted" section, the HTTP table row for `POST /mcp`, and the snippets.
- Modify: `docs/design/0002-decision-log.md` with D109.
- Modify: `docs/design/0010-hosted-mcp-endpoint.md` with status `accepted, built in <PR>`.

- [ ] **Step 1: Write `docs/hosting.md`**, with these sections in order:
  1. **VM:** Azure Linux, with the data dir on a Premium SSD v2 managed disk, never local NVMe. Include `az` commands with placeholders, and no real names or IPs.
  2. **DNS.**
  3. **Caddy:** a `Caddyfile` with `reverse_proxy 127.0.0.1:<port>` and `header_up Host 127.0.0.1`.
  4. **`atticdb serve` as a systemd unit:** `--listen 127.0.0.1:<port>` and `--exit-on-poison`, with `Restart=always`.
  5. **Add a tenant:** `atticdb init`, or an edit to `atticdb.json`, then a restart.
  6. **Rotate a token.**
  7. **First-user check:** connect each of the 5 apps and run `fact_set` and `fact_get`.
  8. **Cost and deallocation.**
- [ ] **Step 2: Write the README snippets** for Copilot CLI, VS Code, Claude Code, Codex and Gemini CLI. Check each app's current docs on the web before writing its snippet, and cite the docs URL in a comment beside it. Each uses `https://<host>/mcp` and `Authorization: Bearer <token>`. Say that `file_ref` and `file_get` are local only. Say that remote agents get no chat-start block and should call `fact_list`.
- [ ] **Step 3: Write D109:** "`atticdb serve` answers MCP at `POST /mcp` with Streamable HTTP sessions. It uses the in-process transport and the same tools as `atticdb mcp`, minus the 2 file tools. Tokens pick tenants, and sessions live in memory. Why: agents on any machine reach 1 tenant with a URL and a token (0010)."
- [ ] **Step 4: Check** the prose rules with `grep -c "—"` on each changed doc. Expect 0.
- [ ] **Step 5: Commit:** `git commit -s -m "docs: hosting runbook, hosted MCP snippets, D109"`
