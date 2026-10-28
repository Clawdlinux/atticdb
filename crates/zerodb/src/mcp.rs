//! `zerodb mcp`: an MCP server on stdio with six tools, `memory_append`,
//! `memory_recent`, `fact_set`, `fact_get`, `fact_list` and `memory_search`, backed by a
//! zerodb server.
//!
//! Newline-delimited JSON-RPC 2.0 on stdin and stdout, logs on stderr. One request at a
//! time, so when stdin closes the call in progress has finished. The model never sees
//! retry keys, tokens, tenants or `if_lsn`.

use crate::config::{read_token_file, Config, Token};
use crate::launch::{Launch, Url};
use crate::outcome::{self, Attempt, Merge, Outcome};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The environment variable that may hold the token.
pub const TOKEN_ENV: &str = "ZERODB_TOKEN";
/// Supported protocol revisions, newest first. Others get the first.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const FIRST_BACKOFF: Duration = Duration::from_millis(50);
const MAX_BACKOFF: Duration = Duration::from_secs(2);
/// Time kept for the request itself: no attempt connects or sends with less than this
/// left before the deadline.
const MIN_ATTEMPT: Duration = Duration::from_secs(2);
/// Longest stdin line. A request body cannot be larger anyway.
const MAX_LINE: u64 = 8 << 20;
/// Longest wait for the client's roots before a call that uses the default session.
const ROOTS_WAIT: Duration = Duration::from_secs(2);

/// A stdin line, or `None` for one longer than [`MAX_LINE`].
type Line = Option<Vec<u8>>;

struct Adapter {
    agent: String,
    session: String,
    /// Whether `--session` set the session, so client roots don't change it.
    session_flag: bool,
    /// Whether the client reports roots, which then name the default session.
    roots: bool,
    /// Requests sent to the client, and the id of the latest `roots/list` not yet answered.
    asked: u64,
    roots_pending: Option<String>,
    /// Calls wait for that answer only until 2 s after it was asked for.
    roots_deadline: Option<Instant>,
    token: Token,
    retry_deadline: Duration,
    launch: Launch,
    initialized: bool,
}

/// Runs the adapter on `zerodb mcp`'s flags until stdin closes. Returns the exit code.
pub fn start(flags: HashMap<String, String>) -> u8 {
    let mut adapter = match Adapter::new(flags) {
        Ok(adapter) => adapter,
        Err(message) => {
            eprintln!("zerodb: {message}");
            return 1;
        }
    };
    // A thread reads stdin, so a call can wait for the roots answer with a timeout.
    let (send, lines) = std::sync::mpsc::sync_channel::<Line>(1);
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        loop {
            let mut line = Vec::new();
            match (&mut stdin).take(MAX_LINE + 1).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let line = if line.len() as u64 > MAX_LINE && !line.ends_with(b"\n") {
                let _ = stdin.skip_until(b'\n');
                None
            } else {
                Some(line)
            };
            if send.send(line).is_err() {
                return;
            }
        }
    });
    // Lines read while a call waited for roots, handled after it in order.
    let (mut queued, mut stdout) = (VecDeque::new(), io::stdout());
    loop {
        let Some(line) = queued.pop_front().or_else(|| lines.recv().ok()) else { return 0 };
        let reply = match line {
            None => Some(rpc_error(Value::Null, -32600, "message longer than 8 MiB")),
            Some(line) if line.iter().all(u8::is_ascii_whitespace) => None,
            Some(line) => {
                if adapter.needs_roots(&line) {
                    // Past 8 MiB in the whole queue, stop reading ahead, so the reader is held
                    // back again. Earlier waits' lines count too.
                    let mut held: u64 =
                        queued.iter().flatten().map(|l: &Vec<u8>| l.len() as u64).sum();
                    while let Some(left) = adapter.roots_wait().filter(|_| held <= MAX_LINE) {
                        match lines.recv_timeout(left) {
                            Ok(Some(next)) if adapter.answers_roots(&next) => {
                                adapter.handle(&next);
                            }
                            Ok(next) => {
                                held += next.as_ref().map_or(0, Vec::len) as u64;
                                queued.push_back(next);
                            }
                            Err(_) => break,
                        }
                    }
                }
                adapter.handle(&line)
            }
        };
        if let Some(reply) = reply {
            let _ = writeln!(stdout, "{reply}").and_then(|()| stdout.flush());
        }
    }
}

/// Runs one tool from a shell, with arguments from `--<name>` flags. Prints the tool's
/// `structuredContent` as one JSON line. Exits 0, 1 when nothing was written or a read
/// failed, 2 for a bad flag, or 3 when a write's outcome is unknown.
pub fn command(tool: &str, mut flags: HashMap<String, String>) -> u8 {
    let mut args = json!({});
    for name in ["content", "role", "key", "prefix", "after", "query"] {
        if let Some(value) = flags.remove(&format!("--{name}")) {
            args[name] = json!(value);
        }
    }
    let numbers =
        [("--n", "n"), ("--limit", "limit"), ("--max-bytes", "max_bytes"), ("--before", "before")];
    for (flag, name) in numbers {
        match flags.remove(flag).map(|text| text.parse::<u64>()) {
            Some(Ok(number)) => args[name] = json!(number),
            Some(Err(_)) => {
                eprintln!("zerodb: {flag} must be a whole number");
                return 2;
            }
            None => {}
        }
    }
    if flags.remove("--delete").is_some() {
        args["delete"] = json!(true);
    }
    let result = match Adapter::new(flags) {
        Ok(mut adapter) => adapter.call(tool, &args).unwrap_or_default(),
        Err(message) => tool_error(message),
    };
    if result["isError"] != true {
        println!("{}", result["structuredContent"]);
        return 0;
    }
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    eprintln!("zerodb: {text}");
    if text.starts_with("unknown:") {
        3
    } else {
        1
    }
}

/// Reads from the per-user home for code in this crate, such as the session-start hook.
pub(crate) struct HomeReader {
    adapter: Adapter,
    deadline: Instant,
}

impl HomeReader {
    /// Resolves the home as `zerodb mcp` with no connection flags does, quietly ignoring
    /// `ZERODB_TOKEN`. All reads together, connects and server starts included, end by
    /// `deadline`.
    pub(crate) fn new(session: &str, deadline: Instant) -> Result<Self, String> {
        let flags = HashMap::from([("--session".into(), session.into())]);
        let adapter = Adapter::open(flags, false)?;
        Ok(Self { adapter, deadline })
    }

    /// Runs the read tool `tool` as `agent`. Returns its `structuredContent`, or its error.
    /// With under 2 s left it sends nothing and fails at once.
    pub(crate) fn read(&mut self, agent: &str, tool: &str, args: &Value) -> Result<Value, String> {
        self.adapter.agent = agent.to_owned();
        // `retry` and every exchange in it end at now + `retry_deadline`.
        self.adapter.retry_deadline = self.deadline.saturating_duration_since(Instant::now());
        let result = self.adapter.call(tool, args).ok_or_else(|| format!("unknown tool {tool}"))?;
        match result["content"][0]["text"].as_str() {
            Some(text) if result["isError"] == true => Err(text.to_owned()),
            _ => Ok(result["structuredContent"].clone()),
        }
    }
}

impl Adapter {
    fn new(flags: HashMap<String, String>) -> Result<Self, String> {
        Self::open(flags, true)
    }

    /// With `note`, says on stderr when the home is used and `ZERODB_TOKEN` is ignored.
    fn open(mut flags: HashMap<String, String>, note: bool) -> Result<Self, String> {
        let url = flags.remove("--url").as_deref().map(Url::parse).transpose()?;
        let mut duration = |name: &str, default: u64| match flags.remove(name) {
            None => Ok(Duration::from_secs(default)),
            Some(text) => match text.strip_suffix("ms") {
                Some(ms) => ms.parse().map(Duration::from_millis).ok(),
                None => {
                    text.strip_suffix('s').and_then(|s| s.parse().ok()).map(Duration::from_secs)
                }
            }
            .ok_or(format!("{name} must be <n>s or <n>ms")),
        };
        let retry_deadline = duration("--retry-deadline", 30)?;
        // With no connection flags, use the per-user home and its token, never the
        // environment's. `--url` was removed above, so it counts through `url`.
        let connection = ["--data-dir", "--config", "--token-file", "--tenant"];
        let given = url.is_some() || connection.iter().any(|flag| flags.contains_key(*flag));
        let home = (!given).then(crate::home::ensure).transpose()?;
        if home.is_some() && std::env::var_os(TOKEN_ENV).is_some() && note {
            eprintln!(
                "zerodb: ignoring {TOKEN_ENV}: no --url, --data-dir or --config given, so the \
                 home's token is used"
            );
        }
        let env_token = std::env::var(TOKEN_ENV).ok().filter(|_| home.is_none());
        // Absolute, as a started server runs in `/`.
        let config_path = flags.remove("--config").map(std::path::absolute).transpose();
        let config_path = config_path.map_err(|error| format!("--config: {error}"))?;
        let config_path = config_path.or_else(|| home.as_ref().map(|home| home.config.clone()));
        let config = config_path.as_deref().map(Config::load).transpose()?;
        let token_file = flags.remove("--token-file");
        let tenant =
            flags.remove("--tenant").or_else(|| home.as_ref().map(|home| home.tenant.clone()));
        let token = match (token_file, env_token, tenant) {
            (Some(path), _, _) => read_token_file(path.as_ref())?,
            (None, Some(text), _) => {
                Token::new(text).map_err(|error| format!("{TOKEN_ENV}: {error}"))?
            }
            (None, None, Some(name)) => config
                .as_ref()
                .and_then(|config| config.tenant(&name))
                .map(|tenant| tenant.token.clone())
                .ok_or_else(|| format!("tenant {name:?} is not in the config"))?,
            _ => return Err(format!("set {TOKEN_ENV}, --token-file, or --config with --tenant")),
        };
        let data_dir = flags.remove("--data-dir").map(PathBuf::from);
        let data_dir = data_dir.or(home.map(|home| home.data_dir));
        let data_dir = data_dir.or_else(|| config.and_then(|config| config.data_dir));
        let data_dir = data_dir.map(std::path::absolute).transpose().map_err(|e| e.to_string())?;
        if url.is_none() && data_dir.is_none() {
            return Err("set --url, or --data-dir or a config with data_dir".into());
        }
        let session = flags.remove("--session");
        Ok(Self {
            agent: flags.remove("--agent").unwrap_or_else(|| "mcp".into()),
            session_flag: session.is_some(),
            // The repo of the current directory, as the session-start hook reads it, until
            // the client's roots name one.
            session: session.unwrap_or_else(|| {
                let repo = std::env::current_dir().ok().and_then(|dir| crate::home::repo(&dir));
                repo.unwrap_or_else(|| "default".into())
            }),
            roots: false,
            asked: 0,
            roots_pending: None,
            roots_deadline: None,
            token,
            retry_deadline,
            launch: Launch::new(url, data_dir, config_path),
            initialized: false,
        })
    }

    /// One JSON-RPC message in, at most one out. Notifications and responses get none.
    fn handle(&mut self, line: &[u8]) -> Option<Value> {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            return Some(rpc_error(Value::Null, -32700, "parse error"));
        };
        let id = message.get("id").cloned();
        // JSON-RPC 2.0: an invalid envelope runs nothing, and a bad id is answered as null.
        let valid_id =
            id.as_ref().is_none_or(|id| id.is_string() || id.is_number() || id.is_null());
        if message["jsonrpc"] != "2.0" || !valid_id {
            let id = id.filter(|_| valid_id).unwrap_or_default();
            return Some(rpc_error(id, -32600, "invalid request"));
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            let response = message.get("result").or(message.get("error")).is_some();
            let answers = id.as_ref().and_then(Value::as_str);
            if response && answers.is_some() && answers == self.roots_pending.as_deref() {
                self.roots_pending = None;
                if let Some(repo) = root_repo(&message["result"]) {
                    self.session = repo;
                }
            }
            return (!response)
                .then(|| rpc_error(id.unwrap_or_default(), -32600, "invalid request"));
        };
        // Notifications, `notifications/cancelled` included: a cancelled write may still
        // commit, which is safe because it carries a key. Roots are asked for once the
        // client is ready, and again when they change.
        let Some(id) = id else {
            let roots = ["notifications/initialized", "notifications/roots/list_changed"];
            return (self.roots && roots.contains(&method)).then(|| self.ask_roots());
        };
        let params = &message["params"];
        let result = match method {
            "ping" => json!({}),
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = VERSIONS.into_iter().find(|v| *v == asked).unwrap_or(VERSIONS[0]);
                self.initialized = true;
                self.roots = !self.session_flag && !params["capabilities"]["roots"].is_null();
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "zerodb", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                })
            }
            "tools/list" if self.initialized => tools(),
            "tools/call" if self.initialized => {
                let name = params["name"].as_str().unwrap_or_default();
                match self.call(name, &params["arguments"]) {
                    Some(mut result) => {
                        safe_integers(&mut result);
                        result
                    }
                    None => return Some(rpc_error(id, -32602, "unknown tool")),
                }
            }
            _ => return Some(rpc_error(id, -32601, "method not found")),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    /// Whether `line` calls a tool that uses the default session while roots may still
    /// change it.
    fn needs_roots(&self, line: &[u8]) -> bool {
        if self.roots_wait().is_none() {
            return false;
        }
        let message: Value = serde_json::from_slice(line).unwrap_or_default();
        let (params, tools) = (&message["params"], ["memory_append", "memory_recent"]);
        message["method"] == "tools/call"
            && tools.iter().any(|tool| params["name"] == *tool)
            && params["arguments"]["session"].is_null()
    }

    /// The time left to wait for the latest `roots/list` answer, if any.
    fn roots_wait(&self) -> Option<Duration> {
        self.roots_pending.as_ref()?;
        let left = self.roots_deadline?.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    /// Whether `line` is a valid answer to the latest `roots/list`, as `handle` takes it.
    fn answers_roots(&self, line: &[u8]) -> bool {
        let message: Value = serde_json::from_slice(line).unwrap_or_default();
        let answer = message.get("result").or(message.get("error")).is_some();
        message["jsonrpc"] == "2.0"
            && message.get("method").is_none()
            && answer
            && message["id"].as_str().is_some_and(|id| Some(id) == self.roots_pending.as_deref())
    }

    /// A `roots/list` request to the client. Only the answer to the latest one counts.
    fn ask_roots(&mut self) -> Value {
        self.asked += 1;
        let id = format!("zerodb-roots-{}", self.asked);
        self.roots_pending = Some(id.clone());
        self.roots_deadline = Some(Instant::now() + ROOTS_WAIT);
        json!({"jsonrpc": "2.0", "id": id, "method": "roots/list"})
    }

    fn call(&mut self, name: &str, args: &Value) -> Option<Value> {
        Some(match name {
            "memory_append" => self.append(args),
            "memory_recent" => self.recent(args),
            "memory_search" => self.search(args),
            "fact_set" => self.fact_set(args),
            "fact_get" => self.fact_get(args),
            "fact_list" => self.fact_list(args),
            _ => return None,
        })
    }

    fn append(&mut self, args: &Value) -> Value {
        let turn = (|| {
            let content = string(args, "content")?.ok_or("content is required")?;
            let role = string(args, "role")?.unwrap_or_else(|| "assistant".into());
            let mut turn = json!({"role": role, "content": content});
            match &args["meta"] {
                Value::Null => {}
                meta @ Value::Object(_) => turn["meta"] = meta.clone(),
                _ => return Err("meta must be an object".to_owned()),
            }
            Ok((string(args, "session")?, turn))
        })();
        let (session, turn) = match turn {
            Ok(turn) => turn,
            Err(message) => return tool_error(message),
        };
        let session = session.unwrap_or_else(|| self.session.clone());
        let body = json!({"agent": self.agent, "session": session, "turns": [turn]});
        self.write("/v1/append", body)
    }

    fn fact_set(&mut self, args: &Value) -> Value {
        let body = (|| {
            let key = string(args, "key")?.ok_or("key is required")?;
            let mut body = json!({"key": key, "agent": self.agent});
            let delete = args["delete"] == true;
            match (string(args, "content")?, delete) {
                (Some(content), false) => body["content"] = json!(content),
                (None, true) => {}
                _ => return Err("pass exactly one of content or delete: true".to_owned()),
            }
            match &args["meta"] {
                Value::Null => {}
                meta @ Value::Object(_) if !delete => body["meta"] = meta.clone(),
                _ => return Err("meta must be an object, and only with content".into()),
            }
            Ok((body, delete))
        })();
        match body {
            Ok((body, true)) => self.write("/v1/facts/delete", body),
            Ok((body, false)) => self.write("/v1/facts/put", body),
            Err(message) => tool_error(message),
        }
    }

    fn fact_get(&mut self, args: &Value) -> Value {
        match string(args, "key").and_then(|key| key.ok_or("key is required".into())) {
            Ok(key) => {
                self.read("fact_get", &format!("/v1/facts/get?key={}", encode(&key)), "fact", fact)
            }
            Err(message) => tool_error(message),
        }
    }

    fn fact_list(&mut self, args: &Value) -> Value {
        let target = (|| {
            let prefix = string(args, "prefix")?.ok_or("prefix is required")?;
            let (limit, max_bytes) =
                (number(args, "limit", 50)?.min(200), number(args, "max_bytes", 64 << 10)?);
            let mut target = format!(
                "/v1/facts/list?prefix={}&limit={limit}&max_bytes={max_bytes}",
                encode(&prefix)
            );
            if let Some(after) = string(args, "after")? {
                target += &format!("&after={}", encode(&after));
            }
            Ok::<_, String>(target)
        })();
        match target {
            Ok(target) => self.read("fact_list", &target, "facts", facts),
            Err(message) => tool_error(message),
        }
    }

    /// Posts a write under one new retry key, judged by the outcome rule across attempts.
    fn write(&mut self, target: &str, mut body: Value) -> Value {
        match crate::random_hex(16) {
            Ok(key) => body["idem_key"] = json!(key),
            Err(error) => return tool_error(format!("cannot read /dev/urandom: {error}")),
        }
        let (mut merge, mut reasons) = (Merge::default(), Vec::new());
        self.retry("POST", target, body.to_string().as_bytes(), |reply| {
            let (attempt, detail) = match &reply {
                Ok(reply) => match reply["outcome"].as_str().and_then(outcome::parse) {
                    Some(outcome) => (Attempt::Answered(outcome), describe(reply)),
                    None => (Attempt::TransportError, "malformed response".into()),
                },
                Err((attempt, error)) => (*attempt, error.clone()),
            };
            let detail = note(&mut reasons, detail);
            match (merge.record(attempt), reply) {
                (Some(Outcome::Committed), Ok(reply)) => Ok(committed(&reply, target)),
                (Some(outcome), _) => Ok(failed(outcome, &detail)),
                (None, _) => Err(failed(merge.finish(), &detail)),
            }
        })
    }

    fn recent(&mut self, args: &Value) -> Value {
        let query = (|| {
            let session = string(args, "session")?.unwrap_or_else(|| self.session.clone());
            let max_bytes = number(args, "max_bytes", 64 << 10)?;
            Ok::<_, String>((number(args, "n", 20)?.min(200), session, max_bytes))
        })();
        let (n, session, max_bytes) = match query {
            Ok(query) => query,
            Err(message) => return tool_error(message),
        };
        let (agent, session) = (encode(&self.agent), encode(&session));
        let target =
            format!("/v1/window?agent={agent}&session={session}&last={n}&max_bytes={max_bytes}");
        self.read("memory_recent", &target, "turns", turns)
    }

    fn search(&mut self, args: &Value) -> Value {
        let target = (|| {
            let query = string(args, "query")?.ok_or("query is required")?;
            let (limit, max_bytes) =
                (number(args, "limit", 10)?.clamp(1, 50), number(args, "max_bytes", 64 << 10)?);
            let mut target =
                format!("/v1/search?q={}&limit={limit}&max_bytes={max_bytes}", encode(&query));
            if !args["before"].is_null() {
                target += &format!("&before={}", number(args, "before", 0)?);
            }
            Ok::<_, String>(target)
        })();
        match target {
            Ok(target) => self.read("memory_search", &target, "hits", hits),
            Err(message) => tool_error(message),
        }
    }

    /// Sends a read until it is answered. The reply field `field` marks success.
    fn read(
        &mut self,
        tool: &str,
        target: &str,
        field: &str,
        render: fn(&Value) -> Value,
    ) -> Value {
        let mut reasons = Vec::new();
        self.retry("GET", target, &[], |reply| match reply {
            Ok(reply) if reply.get(field).is_some() => Ok(render(&reply)),
            Ok(reply) => {
                let detail = note(&mut reasons, describe(&reply));
                Ok(tool_error(format!("{tool} failed: {detail}")))
            }
            Err((_, error)) => {
                Err(tool_error(format!("{tool} failed: {}", note(&mut reasons, error))))
            }
        })
    }

    /// Sends one request, reconnecting before each attempt, until `judge` returns `Ok`.
    /// `Err` holds the result to report if the deadline passes first.
    fn retry(
        &mut self,
        method: &str,
        target: &str,
        body: &[u8],
        mut judge: impl FnMut(Result<Value, (Attempt, String)>) -> Result<Value, Value>,
    ) -> Value {
        let deadline = Instant::now() + self.retry_deadline;
        let mut backoff = FIRST_BACKOFF;
        loop {
            let reply = self.request(method, target, body, deadline);
            match judge(reply) {
                Ok(result) => return result,
                Err(result) if self.launch.stuck() => return result,
                Err(result) if Instant::now() + backoff + MIN_ATTEMPT > deadline => return result,
                Err(_) => std::thread::sleep(jitter(backoff)),
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// One attempt, connect included, bounded by the call's deadline.
    fn request(
        &mut self,
        method: &str,
        target: &str,
        body: &[u8],
        deadline: Instant,
    ) -> Result<Value, (Attempt, String)> {
        let connect_by = deadline.checked_sub(MIN_ATTEMPT).unwrap_or(deadline);
        // Checked before connecting and before sending. Nothing sent merges as
        // not_committed, which never clears an earlier unknown.
        let reserve = || {
            let late = (Attempt::NotSent, "less than 2 s left before the deadline".to_owned());
            (Instant::now() < connect_by).then_some(()).ok_or(late)
        };
        reserve()?;
        let (mut stream, host) =
            self.launch.connect(connect_by).map_err(|e| (Attempt::NotSent, e.to_string()))?;
        reserve()?;
        let auth = format!("Bearer {}", self.token.expose());
        let headers = [("Host", host.as_str()), ("Authorization", auth.as_str())];
        let lost = |error: String| (Attempt::TransportError, error);
        let max = crate::api::MAX_RESPONSE_BYTES;
        let response =
            crate::http::exchange(&mut *stream, method, target, &headers, body, deadline, max);
        let response = response.map_err(|error| lost(error.to_string()))?;
        serde_json::from_slice(&response.body).map_err(|_| lost("response is not JSON".into()))
    }
}

/// Sent in `initialize`. Clients may add it to the system prompt.
const INSTRUCTIONS: &str = "zerodb is this user's durable memory. Memory may arrive in a \
    \"zerodb memory\" block at chat start. Use it, and call memory_search or fact_list only \
    for more. <repo> is the name of the folder that holds .git, as in the block. Save rules \
    and decisions with fact_set under user/<topic> or repo/<repo>/<area>/<topic>. After a \
    meaningful task, save 1 short memory_append summary without a session argument. Never \
    store secrets.";

/// Title and whether the tool only reads. VS Code runs read-only tools without asking.
const ANNOTATIONS: [(&str, &str, bool); 6] = [
    ("memory_append", "Save a turn", false),
    ("memory_recent", "Read recent turns", true),
    ("fact_set", "Save or delete a fact", false),
    ("fact_get", "Read a fact", true),
    ("fact_list", "List facts", true),
    ("memory_search", "Search memory", true),
];

fn tools() -> Value {
    let mut list = tool_schemas();
    for tool in list["tools"].as_array_mut().into_iter().flatten() {
        if let Some((_, title, read_only)) = ANNOTATIONS.iter().find(|(n, ..)| tool["name"] == *n) {
            // Writes only add or replace, never touch anything outside zerodb.
            let destructive = tool["name"] == "fact_set";
            tool["annotations"] = json!({"title": title, "readOnlyHint": read_only,
                "destructiveHint": destructive, "openWorldHint": false});
        }
    }
    list
}

fn tool_schemas() -> Value {
    json!({"tools": [
        {
            "name": "memory_append",
            "description": "Save one turn to memory. Returns its sequence number.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": {"type": "string"},
                    "role": {"type": "string", "enum": ["system", "user", "assistant", "tool"],
                             "default": "assistant"},
                    "session": {"type": "string"},
                    "meta": {"type": "object"},
                },
                "required": ["content"],
            },
        },
        {
            "name": "memory_recent",
            "description": "Read the most recent turns of a session, oldest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "n": {"type": "integer", "minimum": 0, "maximum": 200, "default": 20},
                    "session": {"type": "string"},
                    "max_bytes": {"type": "integer", "minimum": 0, "default": 65536},
                },
            },
        },
        {
            "name": "fact_set",
            "description": "Save a rule or fact under a path-like key, for example \
                repo/payments/api/retry-rule. It replaces the key's current value, so a \
                correction supersedes the old rule. Pass delete: true to remove the key. Every \
                agent using this memory shares the same keys.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "content": {"type": "string"},
                    "delete": {"type": "boolean"},
                    "meta": {"type": "object"},
                },
                "required": ["key"],
            },
        },
        {
            "name": "fact_get",
            "description": "Read the current value of one key. A deleted key returns nothing, \
                never an older value.",
            "inputSchema": {
                "type": "object",
                "properties": {"key": {"type": "string"}},
                "required": ["key"],
            },
        },
        {
            "name": "fact_list",
            "description": "List current facts whose keys start with prefix, in key order, with \
                their content. Call it before working in an area, for example with prefix \
                repo/payments/.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prefix": {"type": "string"},
                    "after": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50},
                    "max_bytes": {"type": "integer", "minimum": 0, "default": 65536},
                },
                "required": ["prefix"],
            },
        },
        {
            "name": "memory_search",
            "description": "Find past turns and facts by words, across every session and agent. \
                Every word must appear, ignoring case, as part of a word or a whole one. Newest \
                first. Use it when you don't know the fact key or the session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 10},
                    "before": {"type": "integer", "minimum": 1},
                    "max_bytes": {"type": "integer", "minimum": 0, "default": 65536},
                },
                "required": ["query"],
            },
        },
    ]})
}

fn number(args: &Value, name: &str, default: u64) -> Result<u64, String> {
    match &args[name] {
        Value::Null => Ok(default),
        value => value.as_u64().ok_or(format!("{name} must be a whole number")),
    }
}

/// A tool result with one text block.
/// Turns integers beyond ±(2^53 - 1), such as `commit_time_ns`, into strings. JavaScript
/// clients can't hold them exactly, and some parse them as `BigInt`, which then fails to
/// serialize. I-JSON (RFC 7493) allows only this range. Shell verbs keep the numbers.
fn safe_integers(value: &mut Value) {
    const MAX: u64 = (1 << 53) - 1;
    match value {
        Value::Number(number) => {
            let too_big = number.as_u64().map(|n| n > MAX);
            if too_big.or_else(|| number.as_i64().map(|n| n < -(MAX as i64))) == Some(true) {
                *value = Value::String(number.to_string());
            }
        }
        Value::Array(items) => items.iter_mut().for_each(safe_integers),
        Value::Object(fields) => fields.values_mut().for_each(safe_integers),
        _ => {}
    }
}

fn answer(text: String, structured: Value) -> Value {
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured})
}

fn string(args: &Value, name: &str) -> Result<Option<String>, String> {
    match &args[name] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(format!("{name} must be a string")),
    }
}

/// Adds a reason once and returns every reason so far, in order. The last alone would
/// depend on timing.
fn note(reasons: &mut Vec<String>, reason: String) -> String {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
    reasons.join("; ")
}

/// The server's error kind and message, for the model.
fn describe(reply: &Value) -> String {
    let error = reply["error"].as_str().unwrap_or("no error kind");
    match reply["message"].as_str() {
        Some(message) => format!("{error}: {message}"),
        None => error.to_owned(),
    }
}

/// One `structuredContent` shape per tool, `seq` for appends only. Receipt fields are null
/// when the server had no receipt.
fn committed(reply: &Value, target: &str) -> Value {
    let append = target == "/v1/append";
    let lsn = &reply[if append { "last_lsn" } else { "lsn" }];
    let mut structured =
        json!({"committed": true, "lsn": lsn, "deduplicated": reply["deduplicated"]});
    let (name, number) = if append { ("seq", &reply["seqs"][0]) } else { ("lsn", lsn) };
    if append {
        structured["seq"] = number.clone();
    }
    let text = match number {
        Value::Null => "committed, receipt unavailable".into(),
        number => format!("committed {name}={number}"),
    };
    answer(text, structured)
}

fn fact(reply: &Value) -> Value {
    let fact = &reply["fact"];
    let text = match fact["content"].as_str() {
        Some("") => "(empty)".into(),
        Some(content) => content.to_owned(),
        None => "no current value".into(),
    };
    answer(text, json!({"fact": fact}))
}

fn facts(reply: &Value) -> Value {
    let facts = reply["facts"].as_array().map(Vec::as_slice).unwrap_or_default();
    let field = |fact: &Value, name: &str| fact[name].as_str().unwrap_or_default().to_owned();
    let mut text: Vec<String> = facts
        .iter()
        .map(|fact| format!("{}: {}", field(fact, "key"), field(fact, "content")))
        .collect();
    let next = reply["next"].as_str();
    if text.is_empty() {
        text.push("no current facts".into());
    }
    if let Some(next) = next {
        text.push(format!("more: call again with after={next}"));
    }
    answer(text.join("\n"), json!({"facts": facts, "next": reply["next"]}))
}

fn hits(reply: &Value) -> Value {
    let hits = reply["hits"].as_array().map(Vec::as_slice).unwrap_or_default();
    let field = |hit: &Value, name: &str| hit[name].as_str().unwrap_or_default().to_owned();
    let mut text: Vec<String> = hits
        .iter()
        .map(|hit| match hit["kind"].as_str() {
            Some("fact") => format!("[fact {}] {}", field(hit, "key"), field(hit, "content")),
            _ => format!(
                "[{}/{} #{} {}] {}",
                field(hit, "agent"),
                field(hit, "session"),
                hit["seq"],
                field(hit, "role"),
                field(hit, "content")
            ),
        })
        .collect();
    if text.is_empty() {
        text.push("no matches".into());
    }
    if let Some(next) = reply["next"].as_u64() {
        text.push(format!("more: call again with before={next}"));
    }
    answer(text.join("\n"), json!({"hits": hits, "next": reply["next"]}))
}

fn failed(outcome: Outcome, detail: &str) -> Value {
    tool_error(match outcome {
        Outcome::NotCommitted => {
            format!("not_committed: nothing was written ({detail}); calling again is safe")
        }
        _ => format!(
            "unknown: the write may be saved, and repeating the call may duplicate it ({detail})"
        ),
    })
}

fn turns(reply: &Value) -> Value {
    let turns = reply["turns"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut text: Vec<String> = turns
        .iter()
        .map(|turn| {
            let content = turn["content"].as_str().unwrap_or_default();
            format!("#{} {}: {content}", turn["seq"], turn["role"].as_str().unwrap_or_default())
        })
        .collect();
    if reply["truncated"] == true {
        text.push("(older turns left out to fit max_bytes)".into());
    }
    let text = if text.is_empty() { "no turns".into() } else { text.join("\n") };
    answer(text, json!({"turns": turns, "truncated": reply["truncated"]}))
}

fn tool_error(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The repo of the first `file://` root in a `roots/list` result, by [`crate::home::repo`].
fn root_repo(result: &Value) -> Option<String> {
    let mut uris = result["roots"].as_array()?.iter().filter_map(|root| root["uri"].as_str());
    let path = uris.find_map(|uri| uri.strip_prefix("file://"))?;
    // `file://localhost/path` names the same path as `file:///path`.
    let path = path.strip_prefix("localhost").unwrap_or(path);
    let (mut bytes, mut decoded) = (path.bytes(), Vec::new());
    while let Some(byte) = bytes.next() {
        decoded.push(match byte {
            b'%' => {
                let hex = [bytes.next()?, bytes.next()?];
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?
            }
            byte => byte,
        });
    }
    // PATH_MAX. A longer path is no real folder, and walking its ancestors costs a stat each.
    if decoded.len() > 4096 {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(decoded).ok()?);
    path.is_absolute().then(|| crate::home::repo(&path)).flatten()
}

/// Percent-encodes a query value.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A random wait between half and all of `backoff`, so retrying adapters spread out.
fn jitter(backoff: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let random = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let half = backoff / 2;
    half + Duration::from_nanos(random % (half.as_nanos() as u64 + 1))
}
