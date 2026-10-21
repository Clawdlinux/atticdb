//! `zerodb mcp`: an MCP server on stdio with two tools, `memory_append` and
//! `memory_recent`, backed by a zerodb server.
//!
//! Newline-delimited JSON-RPC 2.0 on stdin and stdout, logs on stderr. One request at a
//! time, so when stdin closes the call in progress has finished. The model never sees
//! keys, tokens or tenants.

use crate::config::{read_token_file, Config, Token};
use crate::launch::{Launch, Url};
use crate::outcome::{Attempt, Merge, Outcome, Step};
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
/// Time kept for the request itself: connecting stops this long before the deadline.
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
        let config = config_path.as_deref().map(Config::load).transpose();
        let config = config.map_err(|error| error.to_string())?;
        let token_file = flags.remove("--token-file");
        let token = match (token_file, std::env::var(TOKEN_ENV), flags.remove("--tenant")) {
            (Some(path), _, _) => read_token_file(path.as_ref()).map_err(|e| e.to_string())?,
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
        let ((session, turn), key) = match (turn, crate::random_hex(16)) {
            (Ok(turn), Ok(key)) => (turn, key),
            (Err(message), _) => return tool_error(message),
            (_, Err(error)) => return tool_error(format!("cannot read /dev/urandom: {error}")),
        };
        let session = session.unwrap_or_else(|| self.session.clone());
        let body =
            json!({"agent": self.agent, "session": session, "idem_key": key, "turns": [turn]});
        let mut merge = Merge::default();
        self.retry("POST", "/v1/append", body.to_string().as_bytes(), |reply| {
            let (attempt, detail) = match &reply {
                Ok(reply) => match reply["outcome"].as_str().and_then(Outcome::parse) {
                    Some(outcome) => (Attempt::Answered(outcome), describe(reply)),
                    None => (Attempt::TransportError, "malformed response".into()),
                },
                Err((attempt, error)) => (*attempt, error.clone()),
            };
            match (merge.record(attempt), reply) {
                (Step::Return(Outcome::Committed), Ok(reply)) => Ok(committed(&reply)),
                (Step::Return(outcome), _) => Ok(failed(outcome, &detail)),
                (Step::Retry, _) => Err(failed(merge.finish(), &detail)),
            }
        })
    }

    fn recent(&mut self, args: &Value) -> Value {
        let query = (|| {
            let number = |name: &str, default: u64| match &args[name] {
                Value::Null => Ok(default),
                value => value.as_u64().ok_or(format!("{name} must be a whole number")),
            };
            let session = string(args, "session")?.unwrap_or_else(|| self.session.clone());
            Ok::<_, String>((number("n", 20)?.min(200), session, number("max_bytes", 64 << 10)?))
        })();
        let (n, session, max_bytes) = match query {
            Ok(query) => query,
            Err(message) => return tool_error(message),
        };
        let (agent, session) = (encode(&self.agent), encode(&session));
        let target =
            format!("/v1/window?agent={agent}&session={session}&last={n}&max_bytes={max_bytes}");
        self.retry("GET", &target, &[], |reply| match reply {
            Ok(reply) if reply["turns"].is_array() => Ok(turns(&reply)),
            Ok(reply) => Ok(tool_error(format!("memory_recent failed: {}", describe(&reply)))),
            Err((_, error)) => Err(tool_error(format!("memory_recent failed: {error}"))),
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
                Err(result) if Instant::now() + backoff >= deadline => return result,
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
        let (mut stream, host) =
            self.launch.connect(connect_by).map_err(|e| (Attempt::NotSent, e.to_string()))?;
        let auth = format!("Bearer {}", self.token.expose());
        let headers = [("Host", host.as_str()), ("Authorization", auth.as_str())];
        let max = crate::http::Limits::default().body_bytes;
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
    ]})
}

fn string(args: &Value, name: &str) -> Result<Option<String>, String> {
    match &args[name] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(format!("{name} must be a string")),
    }
}

/// The server's error kind and message, for the model.
fn describe(reply: &Value) -> String {
    let error = reply["error"].as_str().unwrap_or("no error kind");
    match reply["message"].as_str() {
        Some(message) => format!("{error}: {message}"),
        None => error.to_owned(),
    }
}

fn committed(reply: &Value) -> Value {
    let (text, structured) = match reply["seqs"].get(0) {
        Some(seq) => (
            format!("committed seq={seq}"),
            json!({"seq": seq, "lsn": reply["last_lsn"], "deduplicated": reply["deduplicated"]}),
        ),
        None => (
            "committed, receipt unavailable".into(),
            json!({"committed": true, "seq": null, "lsn": null, "deduplicated": null}),
        ),
    };
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured})
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
    let structured = json!({"turns": turns, "truncated": reply["truncated"]});
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured})
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
