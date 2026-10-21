//! HTTP routes: `POST /v1/append`, `GET /v1/window` and `GET /healthz`.

use crate::auth::{self, Auth};
use crate::http::{Head, Response};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use zerodb_core::{
    Appended, Engine, EngineError, ErrorKind, Health, NewTurn, Role, Scope, Turn, WindowQuery,
};
use zerodb_tape::{Error, Outcome};

/// Largest `last` and `limit` of a window.
pub const MAX_WINDOW_TURNS: usize = 1000;
/// `max_bytes` of a window when the query does not set it.
pub const DEFAULT_WINDOW_BYTES: usize = 1 << 20;
/// Largest `max_bytes` of a window.
pub const MAX_WINDOW_BYTES: usize = 8 << 20;

const ROLES: [(Role, &str); 4] = [
    (Role::System, "system"),
    (Role::User, "user"),
    (Role::Assistant, "assistant"),
    (Role::Tool, "tool"),
];

/// A route whose head passed the `Host` and auth checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    /// `GET /healthz`, no token.
    Health,
    /// `POST /v1/append`.
    Append,
    /// `GET /v1/window`.
    Window,
}

/// Checks a head before its body is read: `Host` on TCP, then the token. Returns the
/// route and the tenant, which comes only from the token.
pub fn check(auth: &Auth, head: &Head, tcp: bool) -> Result<(Route, Option<String>), Response> {
    if tcp && !head.header("host").is_some_and(loopback_host) {
        return Err(Response::error(403, "forbidden_host"));
    }
    if (head.method.as_str(), head.path()) == ("GET", "/healthz") {
        return Ok((Route::Health, None));
    }
    let tenant = auth.tenant(head).ok_or_else(auth::unauthorized)?.to_owned();
    match (head.method.as_str(), head.path()) {
        ("POST", "/v1/append") => Ok((Route::Append, Some(tenant))),
        ("GET", "/v1/window") => Ok((Route::Window, Some(tenant))),
        (_, "/v1/append" | "/v1/window" | "/healthz") => {
            Err(Response::error(405, "method_not_allowed"))
        }
        _ => Err(Response::error(404, "not_found")),
    }
}

/// `127.0.0.1`, `localhost` or `[::1]`, with an optional port.
pub(crate) fn loopback_host(value: &[u8]) -> bool {
    let text = String::from_utf8_lossy(value).to_ascii_lowercase();
    let (host, port) = match text.strip_prefix("[::1]") {
        Some(port) => ("[::1]", port),
        None => text.split_at(text.find(':').unwrap_or(text.len())),
    };
    let port_ok = port.is_empty()
        || port.strip_prefix(':').is_some_and(|digits| {
            (1..=5).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit())
        });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]") && port_ok
}

/// `GET /healthz`. Read-only stays 200 on purpose: only a write can clear it.
pub fn health(engine: &Engine, instance: &str) -> Response {
    let (status, text) = match engine.health() {
        Health::Ok => (200, "ok"),
        Health::ReadOnly => (200, "read_only"),
        Health::Poisoned => (503, "poisoned"),
    };
    Response::json(status, &json!({"status": text, "instance": instance}))
}

/// `POST /v1/append`. The raw body is dropped once parsed.
pub fn append(engine: &Engine, tenant: &str, body: Vec<u8>) -> Response {
    let parsed = serde_json::from_slice(&body)
        .map_err(|_| "body must be JSON".to_owned())
        .and_then(parse_append);
    drop(body);
    match parsed {
        Ok((agent, session, key, turns)) => {
            let scope = Scope { tenant: tenant.to_owned(), agent, session };
            append_response(engine.append(&scope, turns, Some(&key)))
        }
        Err(message) => Response::json(
            400,
            &json!({"outcome": "not_committed", "error": "invalid_input", "message": message}),
        ),
    }
}

fn string(fields: &mut Map<String, Value>, name: &str) -> Result<String, String> {
    match fields.remove(name) {
        Some(Value::String(text)) => Ok(text),
        _ => Err(format!("{name} must be a string")),
    }
}

fn parse_append(value: Value) -> Result<(String, String, String, Vec<NewTurn>), String> {
    let Value::Object(mut fields) = value else {
        return Err("body must be a JSON object".into());
    };
    let agent = string(&mut fields, "agent")?;
    let session = string(&mut fields, "session")?;
    let key = string(&mut fields, "idem_key")?;
    if !(1..=128).contains(&key.len()) {
        return Err("idem_key must be 1 to 128 bytes".into());
    }
    let Some(Value::Array(items)) = fields.remove("turns") else {
        return Err("turns must be an array".into());
    };
    let turns = items
        .into_iter()
        .map(|item| {
            let Value::Object(mut turn) = item else {
                return Err("each turn must be an object".to_owned());
            };
            let role = string(&mut turn, "role")?;
            let Some(&(role, _)) = ROLES.iter().find(|(_, name)| *name == role) else {
                return Err("role must be system, user, assistant or tool".into());
            };
            let content = string(&mut turn, "content")?.into_bytes();
            let meta = match turn.remove("meta") {
                None => Vec::new(),
                Some(meta @ Value::Object(_)) => meta.to_string().into_bytes(),
                Some(_) => return Err("meta must be an object".into()),
            };
            Ok(NewTurn { role, content, meta })
        })
        .collect::<Result<_, _>>()?;
    Ok((agent, session, key, turns))
}

/// Maps an engine append result to its response. The status follows the outcome
/// first, so a durable write is always 200, with `receipt: null` when the engine
/// failed after the commit.
pub fn append_response(result: Result<Appended, EngineError>) -> Response {
    let error = match result {
        Ok(receipt) => {
            return Response::json(
                200,
                &json!({
                    "outcome": "committed",
                    "deduplicated": receipt.deduplicated,
                    "txn": receipt.txn,
                    "first_lsn": receipt.first_lsn,
                    "last_lsn": receipt.last_lsn,
                    "seqs": receipt.seqs,
                }),
            )
        }
        Err(error) => error,
    };
    let (status, name) = classify(&error.kind);
    let outcome = match error.outcome {
        Outcome::Committed => {
            let body = json!({"outcome": "committed", "receipt": null, "error": name});
            return Response::json(200, &body);
        }
        Outcome::NotCommitted => "not_committed",
        Outcome::Unknown => "unknown",
    };
    let mut body = json!({"outcome": outcome, "error": name});
    match error.kind {
        ErrorKind::ResourceExhausted(resource) => body["resource"] = json!(resource),
        ErrorKind::InvalidInput => body["message"] = json!(LIMITS),
        _ => {}
    }
    Response::json(status, &body)
}

const LIMITS: &str = "turns must not be empty; content is at most 4 MiB, meta at most 64 KiB, \
                      agent and session at most 255 bytes each";

fn classify(kind: &ErrorKind) -> (u16, &'static str) {
    match kind {
        ErrorKind::InvalidInput => (400, "invalid_input"),
        ErrorKind::IdempotencyConflict => (409, "idempotency_conflict"),
        ErrorKind::ResourceExhausted(_) => (429, "resource_exhausted"),
        ErrorKind::Poisoned => (503, "poisoned"),
        ErrorKind::CorruptIndex => (500, "corrupt_index"),
        ErrorKind::Storage(Error::BatchTooLarge) => (413, "too_large"),
        ErrorKind::Storage(Error::DiskFull) => (507, "disk_full"),
        ErrorKind::Storage(_) => (500, "storage"),
    }
}

/// `GET /v1/window?agent=&session=&last=N` or `...&since_seq=S&limit=N`, with an
/// optional `max_bytes`. Turns are never cut.
pub fn window(engine: &Engine, tenant: &str, target: &str) -> Response {
    let (scope, query, max_bytes) = match parse_window(tenant, target) {
        Ok(parsed) => parsed,
        Err(message) => {
            return Response::json(400, &json!({"error": "invalid_input", "message": message}))
        }
    };
    match engine.window_budget(&scope, query, max_bytes) {
        Ok(window) => {
            let turns: Vec<Value> = window.turns.into_iter().map(turn_json).collect();
            Response::json(200, &json!({"turns": turns, "truncated": window.truncated}))
        }
        Err(error) => {
            let (status, name) = classify(&error.kind);
            Response::json(status, &json!({"error": name}))
        }
    }
}

fn parse_window(tenant: &str, target: &str) -> Result<(Scope, WindowQuery, usize), String> {
    let mut params = BTreeMap::new();
    let query = target.split_once('?').map_or("", |(_, query)| query);
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (Some(name), Some(value)) = (decode(name), decode(value)) else {
            return Err("query must be percent-encoded UTF-8".into());
        };
        if params.insert(name, value).is_some() {
            return Err("a query parameter is repeated".into());
        }
    }
    let mut take = |name: &str| params.remove(name);
    let number = |value: Option<String>, name: &str| {
        value.map(|text| text.parse::<u64>().map_err(|_| format!("{name} must be a number")))
    };
    let (Some(agent), Some(session)) = (take("agent"), take("session")) else {
        return Err("agent and session are required".into());
    };
    let cap = |value: u64, max: usize| usize::try_from(value).unwrap_or(max).min(max);
    let last = number(take("last"), "last").transpose()?;
    let since = number(take("since_seq"), "since_seq").transpose()?;
    let limit = number(take("limit"), "limit").transpose()?;
    let max_bytes = number(take("max_bytes"), "max_bytes").transpose()?;
    let query = match (last, since, limit) {
        (Some(last), None, None) => WindowQuery::Last(cap(last, MAX_WINDOW_TURNS)),
        (None, Some(seq), limit) => WindowQuery::Since {
            seq,
            limit: cap(limit.unwrap_or(MAX_WINDOW_TURNS as u64), MAX_WINDOW_TURNS),
        },
        _ => return Err("pass last, or since_seq with an optional limit".into()),
    };
    if !params.is_empty() {
        return Err("unknown query parameter".into());
    }
    let max_bytes = max_bytes.map_or(DEFAULT_WINDOW_BYTES, |bytes| cap(bytes, MAX_WINDOW_BYTES));
    Ok((Scope { tenant: tenant.to_owned(), agent, session }, query, max_bytes))
}

/// Percent-decoding, with `+` as a space.
fn decode(text: &str) -> Option<String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        out.push(match byte {
            b'+' => b' ',
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
    String::from_utf8(out).ok()
}

/// Content that is not UTF-8, only possible from direct Rust API writes, comes back
/// with U+FFFD and `"lossy": true`. Meta is the stored JSON object, or `null`.
fn turn_json(turn: Turn) -> Value {
    let mut lossy = false;
    let mut text = |bytes: Vec<u8>| {
        String::from_utf8(bytes).unwrap_or_else(|error| {
            lossy = true;
            String::from_utf8_lossy(error.as_bytes()).into_owned()
        })
    };
    let meta = match serde_json::from_slice(&turn.meta) {
        _ if turn.meta.is_empty() => Value::Null,
        Ok(meta) => meta,
        Err(_) => Value::String(text(turn.meta)),
    };
    let role = match turn.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut out = json!({
        "seq": turn.seq,
        "lsn": turn.lsn,
        "commit_time_ns": turn.commit_time_ns,
        "role": role,
        "content": text(turn.content),
        "meta": meta,
    });
    if lossy {
        out["lossy"] = json!(true);
    }
    out
}
