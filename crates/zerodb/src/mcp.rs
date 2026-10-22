//! `zerodb mcp`: an MCP server on stdio with five tools, `memory_append`,
//! `memory_recent`, `fact_set`, `fact_get` and `fact_list`, backed by a zerodb server.
//!
//! Newline-delimited JSON-RPC 2.0 on stdin and stdout, logs on stderr. One request at a
//! time, so when stdin closes the call in progress has finished. The model never sees
//! retry keys, tokens, tenants or `if_lsn`.

use crate::config::{read_token_file, Config, Token};
use crate::launch::{Launch, Url};
use crate::outcome::{self, Attempt, Merge, Outcome};
use serde_json::{json, Value};
use std::collections::HashMap;
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

struct Adapter {
    agent: String,
    session: String,
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
    let (mut stdin, mut stdout, mut line) = (io::stdin().lock(), io::stdout(), Vec::new());
    loop {
        line.clear();
        match (&mut stdin).take(MAX_LINE + 1).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return 0,
            Ok(_) => {}
        }
        let reply = if line.len() as u64 > MAX_LINE && !line.ends_with(b"\n") {
            let _ = stdin.skip_until(b'\n');
            Some(rpc_error(Value::Null, -32600, "message longer than 8 MiB"))
        } else if line.iter().all(u8::is_ascii_whitespace) {
            None
        } else {
            adapter.handle(&line)
        };
        if let Some(reply) = reply {
            let _ = writeln!(stdout, "{reply}").and_then(|()| stdout.flush());
        }
    }
}

impl Adapter {
    fn new(mut flags: HashMap<String, String>) -> Result<Self, String> {
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
        let config_path = flags.remove("--config").map(PathBuf::from);
        let config = config_path.as_deref().map(Config::load).transpose()?;
        let token_file = flags.remove("--token-file");
        let token = match (token_file, std::env::var(TOKEN_ENV), flags.remove("--tenant")) {
            (Some(path), _, _) => read_token_file(path.as_ref())?,
            (None, Ok(text), _) => {
                Token::new(text).map_err(|error| format!("{TOKEN_ENV}: {error}"))?
            }
            (None, Err(_), Some(name)) => config
                .as_ref()
                .and_then(|config| config.tenant(&name))
                .map(|tenant| tenant.token.clone())
                .ok_or_else(|| format!("tenant {name:?} is not in the config"))?,
            _ => return Err(format!("set {TOKEN_ENV}, --token-file, or --config with --tenant")),
        };
        let data_dir = flags.remove("--data-dir").map(PathBuf::from);
        let data_dir = data_dir.or_else(|| config.and_then(|config| config.data_dir));
        let data_dir = data_dir.map(std::path::absolute).transpose().map_err(|e| e.to_string())?;
        if url.is_none() && data_dir.is_none() {
            return Err("set --url, or --data-dir or a config with data_dir".into());
        }
        Ok(Self {
            agent: flags.remove("--agent").unwrap_or_else(|| "mcp".into()),
            session: flags.remove("--session").unwrap_or_else(|| "default".into()),
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
            return (!response)
                .then(|| rpc_error(id.unwrap_or_default(), -32600, "invalid request"));
        };
        // Notifications, `notifications/cancelled` included: a cancelled write may still
        // commit, which is safe because it carries a key.
        let id = id?;
        let params = &message["params"];
        let result = match method {
            "ping" => json!({}),
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = VERSIONS.into_iter().find(|v| *v == asked).unwrap_or(VERSIONS[0]);
                self.initialized = true;
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "zerodb", "version": env!("CARGO_PKG_VERSION")},
                })
            }
            "tools/list" if self.initialized => tools(),
            "tools/call" if self.initialized => match params["name"].as_str() {
                Some("memory_append") => self.append(&params["arguments"]),
                Some("memory_recent") => self.recent(&params["arguments"]),
                Some("fact_set") => self.fact_set(&params["arguments"]),
                Some("fact_get") => self.fact_get(&params["arguments"]),
                Some("fact_list") => self.fact_list(&params["arguments"]),
                _ => return Some(rpc_error(id, -32602, "unknown tool")),
            },
            _ => return Some(rpc_error(id, -32601, "method not found")),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
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
        // JSON escapes a byte to at most 6 (`\u0000`), plus 1 MiB for the rest of a window.
        let max = 6 * crate::api::MAX_WINDOW_BYTES as u64 + (1 << 20);
        let lost = |error: String| (Attempt::TransportError, error);
        let response =
            crate::http::exchange(&mut *stream, method, target, &headers, body, deadline, max);
        let response = response.map_err(|error| lost(error.to_string()))?;
        serde_json::from_slice(&response.body).map_err(|_| lost("response is not JSON".into()))
    }
}

fn tools() -> Value {
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
    ]})
}

fn number(args: &Value, name: &str, default: u64) -> Result<u64, String> {
    match &args[name] {
        Value::Null => Ok(default),
        value => value.as_u64().ok_or(format!("{name} must be a whole number")),
    }
}

/// A tool result with one text block.
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
        let empty =
            if next.is_some() { "no current facts in this page" } else { "no current facts" };
        text.push(empty.into());
    }
    if let Some(next) = next {
        text.push(format!("more: call again with after={next}"));
    }
    answer(text.join("\n"), json!({"facts": facts, "next": reply["next"]}))
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
