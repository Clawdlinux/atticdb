//! HTTP routes. Writes: `POST /v1/append`, `POST /v1/facts/put` and `POST /v1/facts/delete`.
//! Reads: `GET /v1/window`, `GET /v1/facts/get`, `GET /v1/facts/history`, `GET /v1/facts/list`,
//! `GET /v1/search`, `GET /v1/sessions` and `GET /v1/records`. `GET /healthz` needs no token.
//! `GET /v1/stats` and `GET /metrics` answer on the Unix socket only, with no token. Fact keys
//! travel in bodies and query strings, never in paths.

use crate::auth::{self, Auth};
use crate::http::{Budget, Head, Response};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use zerodb_core::{
    Appended, Engine, EngineError, ErrorKind, FactQuery, FactWritten, Health, Hit, NewFact,
    NewTurn, Record, RecordKind, RecordQuery, Revision, Role, Scope, SearchQuery, SessionQuery,
    StoredFact, Turn, WindowQuery,
};
use zerodb_tape::{Error, Outcome};

/// Largest `last` and `limit` of a window.
pub const MAX_WINDOW_TURNS: usize = 1000;
/// `max_bytes` of a window when the query does not set it.
pub const DEFAULT_WINDOW_BYTES: usize = 1 << 20;
/// Largest `max_bytes` of a window.
pub const MAX_WINDOW_BYTES: usize = 8 << 20;
/// Records one search call visits at most. A walk costs about 1.3 µs per record on macOS,
/// so a call stays near 1.3 s. `next` resumes below them.
pub const SEARCH_VISITS: usize = 1_000_000;
/// Largest response body. JSON escapes a budgeted byte to at most 6 (`\u0000`). Fixed fields
/// cost at most 151 bytes per item, so 4,096 items and a 6 KiB `next` fit in the 1 MiB rest.
pub const MAX_RESPONSE_BYTES: u64 = 6 * MAX_WINDOW_BYTES as u64 + (1 << 20);

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
    /// `GET /v1/stats`, Unix socket only, no token.
    Stats,
    /// `GET /metrics`, Unix socket only, no token.
    Metrics,
    /// `POST /v1/append`.
    Append,
    /// `GET /v1/window`.
    Window,
    /// `POST /v1/facts/put`.
    FactPut,
    /// `POST /v1/facts/delete`.
    FactDelete,
    /// `GET /v1/facts/get`.
    FactGet,
    /// `GET /v1/facts/history`.
    FactHistory,
    /// `GET /v1/facts/list`.
    FactList,
    /// `GET /v1/search`.
    Search,
    /// `GET /v1/sessions`.
    Sessions,
    /// `GET /v1/records`.
    Records,
}

const ROUTES: [(&str, &str, Route); 11] = [
    ("GET", "/healthz", Route::Health),
    ("POST", "/v1/append", Route::Append),
    ("GET", "/v1/window", Route::Window),
    ("POST", "/v1/facts/put", Route::FactPut),
    ("POST", "/v1/facts/delete", Route::FactDelete),
    ("GET", "/v1/facts/get", Route::FactGet),
    ("GET", "/v1/facts/history", Route::FactHistory),
    ("GET", "/v1/facts/list", Route::FactList),
    ("GET", "/v1/search", Route::Search),
    ("GET", "/v1/sessions", Route::Sessions),
    ("GET", "/v1/records", Route::Records),
];

/// Checks a head before its body is read: `Host` on TCP, then the token. Returns the
/// route and the tenant, which comes only from the token.
pub fn check(auth: &Auth, head: &Head, tcp: bool) -> Result<(Route, Option<String>), Response> {
    if tcp && !head.header("host").is_some_and(loopback_host) {
        return Err(Response::error(403, "forbidden_host", None));
    }
    if (head.method.as_str(), head.path()) == ("GET", "/healthz") {
        return Ok((Route::Health, None));
    }
    // Stats span all tenants, so socket permissions guard them, not a tenant token.
    match (tcp, head.method.as_str(), head.path()) {
        (false, "GET", "/v1/stats") => return Ok((Route::Stats, None)),
        (false, "GET", "/metrics") => return Ok((Route::Metrics, None)),
        _ => {}
    }
    let tenant = auth.tenant(head).ok_or_else(auth::unauthorized)?.to_owned();
    match ROUTES.iter().find(|(_, path, _)| *path == head.path()) {
        Some((method, _, route)) if *method == head.method => Ok((*route, Some(tenant))),
        Some(_) => Err(Response::error(405, "method_not_allowed", None)),
        None => Err(Response::error(404, "not_found", None)),
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
    let (status, text) = health_status(engine);
    Response::json(status, &json!({"status": text, "instance": instance}))
}

fn health_status(engine: &Engine) -> (u16, &'static str) {
    match engine.health() {
        Health::Ok => (200, "ok"),
        Health::ReadOnly => (200, "read_only"),
        Health::Poisoned => (503, "poisoned"),
    }
}

/// `GET /v1/stats`: health and engine metrics since this open.
pub fn stats(engine: &Engine, instance: &str) -> Response {
    let Some(mut fields) = stats_fields(engine) else {
        return Response::error(503, "poisoned", None);
    };
    fields["instance"] = json!(instance);
    Response::json(200, &fields)
}

/// `/v1/stats` counters. Its other numbers are gauges, and other `_ns` maxima become seconds.
const COUNTERS: [&str; 17] = [
    "writer_groups",
    "writer_group_requests",
    "superblock_installs",
    "replayed_records",
    "window_reads",
    "fact_reads",
    "checkpoint_saves",
    "checkpoint_warnings",
    "tape_index_page_reloads",
    "tape_index_page_evictions",
    "tape_index_dirty_page_evictions",
    "busy_replies",
    "turn_repeats",
    "writer_write_ns",
    "writer_sync_ns",
    "writer_publish_ns",
    "writer_reply_ns",
];

/// A Prometheus histogram over fixed upper bounds, with one more bucket past the last.
struct Histogram {
    bounds: &'static [f64],
    counts: [AtomicU64; 24],
    /// Sum in steps of `unit`, so it stays an integer.
    sum: AtomicU64,
    unit: f64,
}

impl Histogram {
    const fn new(bounds: &'static [f64], unit: f64) -> Self {
        Self { bounds, counts: [const { AtomicU64::new(0) }; 24], sum: AtomicU64::new(0), unit }
    }

    fn record(&self, value: f64) {
        let bucket = self.bounds.iter().position(|le| value <= *le).unwrap_or(self.bounds.len());
        self.counts[bucket].fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add((value / self.unit) as u64, Ordering::Relaxed);
    }

    fn render(&self, name: &str, out: &mut String) {
        *out += &format!("# TYPE zerodb_{name} histogram\n");
        let mut count = 0;
        for (i, bucket) in self.counts[..=self.bounds.len()].iter().enumerate() {
            count += bucket.load(Ordering::Relaxed);
            let le = self.bounds.get(i).map_or("+Inf".into(), f64::to_string);
            *out += &format!("zerodb_{name}_bucket{{le=\"{le}\"}} {count}\n");
        }
        let sum = self.sum.load(Ordering::Relaxed) as f64 * self.unit;
        *out += &format!("zerodb_{name}_sum {sum}\nzerodb_{name}_count {count}\n");
    }
}

/// Committed append latency, 50 µs to 1 s.
static ACK_SECONDS: Histogram = Histogram::new(
    &[50e-6, 100e-6, 250e-6, 500e-6, 1e-3, 2.5e-3, 5e-3, 10e-3, 25e-3, 50e-3, 100e-3, 250e-3, 1.0],
    1e-9,
);
/// Content size of newly written turns, 64 B to 4 MiB in powers of 2.
static TURN_BYTES: Histogram = Histogram::new(
    &[
        64.0, 128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0, 8192.0, 16384.0, 32768.0, 65536.0,
        131072.0, 262144.0, 524288.0, 1048576.0, 2097152.0, 4194304.0,
    ],
    1.0,
);
/// One content hash per slot. A turn whose hash is already in its slot is a repeat. Slots
/// get overwritten, so `turn_repeats` is a lower bound on exact repeats.
static RECENT: [AtomicU64; 4096] = [const { AtomicU64::new(0) }; 4096];
static REPEATS: AtomicU64 = AtomicU64::new(0);

/// `GET /metrics`: `/v1/stats` and the append ack histogram in Prometheus text format 0.0.4.
pub fn metrics(engine: &Engine) -> Response {
    let Some(fields) = stats_fields(engine) else {
        return Response::error(503, "poisoned", None);
    };
    let mut out = String::new();
    let mut add = |name: &str, kind: &str, value: &str| {
        out += &format!("# TYPE zerodb_{name} {kind}\nzerodb_{name} {value}\n");
    };
    for (key, value) in fields.as_object().into_iter().flatten() {
        let Some(number) = value.as_u64() else { continue };
        let counter = COUNTERS.contains(&key.as_str());
        match key.strip_suffix("_ns") {
            Some(base) if counter => {
                add(&format!("{base}_seconds_total"), "counter", &(number as f64 / 1e9).to_string())
            }
            Some(base) => {
                add(&format!("{base}_seconds"), "gauge", &(number as f64 / 1e9).to_string())
            }
            None if counter => add(&format!("{key}_total"), "counter", &number.to_string()),
            None => add(key, "gauge", &number.to_string()),
        }
    }
    add("read_only", "gauge", if fields["health"] == "read_only" { "1" } else { "0" });
    ACK_SECONDS.render("append_ack_seconds", &mut out);
    TURN_BYTES.render("turn_content_bytes", &mut out);
    Response::text(200, "text/plain; version=0.0.4; charset=utf-8", out)
}

/// The `GET /v1/stats` fields except `instance`. `None` when the engine is poisoned.
pub fn stats_fields(engine: &Engine) -> Option<Value> {
    let m = engine.metrics().ok()?;
    let index = &m.tape_index;
    Some(json!({
        "health": health_status(engine).1,
        "durable_lsn": m.durable_lsn,
        "tenants": m.tenants,
        "facts": m.facts,
        "fact_revisions": m.fact_revisions,
        "dedup_entries": m.dedup_entries,
        "dedup_window_effective": m.dedup_window_effective,
        "hot_page_bytes": m.hot_page_bytes,
        "writer_groups": m.writer_groups,
        "writer_group_requests": m.writer_group_requests,
        "writer_write_ns": m.writer_stages.write_ns,
        "writer_sync_ns": m.writer_stages.sync_ns,
        "writer_publish_ns": m.writer_stages.publish_ns,
        "writer_reply_ns": m.writer_stages.reply_ns,
        "writer_write_max_ns": m.writer_stages.write_max_ns,
        "writer_sync_max_ns": m.writer_stages.sync_max_ns,
        "writer_publish_max_ns": m.writer_stages.publish_max_ns,
        "busy_replies": BUSY.load(Ordering::Relaxed),
        "turn_repeats": REPEATS.load(Ordering::Relaxed),
        "superblock_installs": m.superblock_installs,
        "replayed_records": m.replayed_records,
        "window_reads": m.window_reads,
        "fact_reads": m.fact_reads,
        "checkpoint_watermark": m.checkpoint_watermark,
        "checkpoint_files": m.checkpoint_files,
        "checkpoint_saves": m.checkpoint_saves,
        "checkpoint_warnings": m.checkpoint_warnings,
        "checkpoint_pending_bytes": m.checkpoint_pending_bytes,
        "checkpoint_buffer_peak_bytes": m.checkpoint_buffer_peak_bytes,
        "checkpoint_snapshot_lock_ns": m.checkpoint_snapshot_lock_ns,
        "checkpoint_state_lock_ns": m.checkpoint_state_lock_ns,
        "tape_index_resident_bytes": index.resident_bytes,
        "tape_index_budget_bytes": index.budget_bytes,
        "tape_index_page_reloads": index.page_reloads,
        "tape_index_page_evictions": index.page_evictions,
        "tape_index_dirty_page_evictions": index.dirty_page_evictions,
    }))
}

/// JSON trees in memory at once: request parses and window builds. An 8 MiB body like
/// `[{},{},...]` grows about 16x as a `Value` tree, so this bounds tree memory, not the
/// connection count.
static PARSERS: Budget = Budget::new(2);
/// Window bytes being written at once.
static SENDING: Budget = Budget::new(256 << 20);
/// How long a window waits for [`SENDING`] before it answers 503 `busy`.
const SEND_WAIT: Duration = Duration::from_secs(5);

/// 503 `busy` replies since the process started. Nonzero means a cap is too small.
static BUSY: AtomicU64 = AtomicU64::new(0);

/// 503 `busy`: nothing reached the engine.
pub(crate) fn busy() -> Response {
    BUSY.fetch_add(1, Ordering::Relaxed);
    Response::error(503, "busy", None)
}

/// `POST /v1/append`. The raw body is dropped once parsed.
pub fn append(engine: &Engine, tenant: &str, body: Vec<u8>) -> Response {
    let Some(slot) = PARSERS.take(1, Duration::MAX) else { return busy() };
    let value: Result<Value, _> = serde_json::from_slice(&body);
    // The tape takes at most 1,024 records per batch. Reject more before encoding, which
    // copies the agent, session and key into every turn outside the parse limit.
    if value.as_ref().is_ok_and(|v| v["turns"].as_array().is_some_and(|t| t.len() > 1024)) {
        return Response::json(413, &json!({"outcome": "not_committed", "error": "too_large"}));
    }
    let parsed = value.map_err(|_| "body must be JSON".to_owned()).and_then(parse_append);
    drop((slot, body));
    match parsed {
        Ok((agent, session, key, turns)) => {
            let scope = Scope { tenant: tenant.to_owned(), agent, session };
            let seen: Vec<(usize, u64)> =
                turns.iter().map(|t| (t.content.len(), hash(&t.content))).collect();
            let started = std::time::Instant::now();
            let result = engine.append(&scope, turns, Some(&key));
            let elapsed = started.elapsed();
            // Dedup and lost receipts tell nothing about new turns.
            let fresh = result.as_ref().is_ok_and(|receipt| !receipt.deduplicated);
            let response = append_response(result);
            if response.status == 200 {
                ACK_SECONDS.record(elapsed.as_secs_f64());
            }
            if fresh {
                for (len, hash) in seen {
                    TURN_BYTES.record(len as f64);
                    if RECENT[(hash % 4096) as usize].swap(hash, Ordering::Relaxed) == hash {
                        REPEATS.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            response
        }
        Err(message) => invalid(message),
    }
}

/// Never 0, the empty slot.
fn hash(content: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish().max(1)
}

fn string(fields: &mut Map<String, Value>, name: &str) -> Result<String, String> {
    match fields.remove(name) {
        Some(Value::String(text)) => Ok(text),
        _ => Err(format!("{name} must be a string")),
    }
}

fn meta(fields: &mut Map<String, Value>) -> Result<Vec<u8>, String> {
    match fields.remove("meta") {
        None => Ok(Vec::new()),
        Some(meta @ Value::Object(_)) => Ok(meta.to_string().into_bytes()),
        Some(_) => Err("meta must be an object".into()),
    }
}

fn idem_key(fields: &mut Map<String, Value>) -> Result<String, String> {
    let key = string(fields, "idem_key")?;
    if !(1..=128).contains(&key.len()) {
        return Err("idem_key must be 1 to 128 bytes".into());
    }
    Ok(key)
}

/// `POST /v1/facts/put` and `POST /v1/facts/delete`. The raw body is dropped once parsed.
pub fn fact_write(engine: &Engine, tenant: &str, body: Vec<u8>, delete: bool) -> Response {
    let Some(slot) = PARSERS.take(1, Duration::MAX) else { return busy() };
    let value = serde_json::from_slice(&body).map_err(|_| "body must be JSON".to_owned());
    let parsed = value.and_then(|value| parse_fact(value, delete));
    drop((slot, body));
    let (fact, key, if_lsn) = match parsed {
        Ok(parsed) => parsed,
        Err(message) => return invalid(message),
    };
    let result = if delete {
        engine.fact_delete(tenant, &fact.key, &fact.agent, Some(&key), if_lsn)
    } else {
        engine.fact_put(tenant, fact, Some(&key), if_lsn)
    };
    let receipt =
        |w: FactWritten| json!({"deduplicated": w.deduplicated, "txn": w.txn, "lsn": w.lsn});
    write_response(result.map(receipt), FACT_LIMITS)
}

fn invalid(message: String) -> Response {
    Response::json(
        400,
        &json!({"outcome": "not_committed", "error": "invalid_input", "message": message}),
    )
}

fn parse_fact(value: Value, delete: bool) -> Result<(NewFact, String, Option<u64>), String> {
    let Value::Object(mut fields) = value else {
        return Err("body must be a JSON object".into());
    };
    let key = string(&mut fields, "key")?;
    let idem = idem_key(&mut fields)?;
    if fields.remove("source_refs").is_some_and(|refs| refs != json!([])) {
        return Err("source_refs must be empty".into());
    }
    let agent =
        if fields.contains_key("agent") { string(&mut fields, "agent")? } else { String::new() };
    let if_lsn =
        fields.remove("if_lsn").map(|lsn| lsn.as_u64().ok_or("if_lsn must be a whole number"));
    let if_lsn = if_lsn.transpose()?;
    let (content, meta) = match delete {
        true => (Vec::new(), Vec::new()),
        false => (string(&mut fields, "content")?.into_bytes(), meta(&mut fields)?),
    };
    Ok((NewFact { key, content, meta, agent }, idem, if_lsn))
}

fn parse_append(value: Value) -> Result<(String, String, String, Vec<NewTurn>), String> {
    let Value::Object(mut fields) = value else {
        return Err("body must be a JSON object".into());
    };
    let agent = string(&mut fields, "agent")?;
    let session = string(&mut fields, "session")?;
    let key = idem_key(&mut fields)?;
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
            Ok(NewTurn { role, content, meta: meta(&mut turn)? })
        })
        .collect::<Result<_, _>>()?;
    Ok((agent, session, key, turns))
}

/// Maps an engine append result to its response. The status follows the outcome
/// first, so a durable write is always 200, with `receipt: null` when the engine
/// failed after the commit.
pub fn append_response(result: Result<Appended, EngineError>) -> Response {
    let receipt = |receipt: Appended| {
        json!({
            "deduplicated": receipt.deduplicated,
            "txn": receipt.txn,
            "first_lsn": receipt.first_lsn,
            "last_lsn": receipt.last_lsn,
            "seqs": receipt.seqs,
        })
    };
    write_response(result.map(receipt), LIMITS)
}

fn write_response(result: Result<Value, EngineError>, limits: &str) -> Response {
    let error = match result {
        Ok(mut receipt) => {
            receipt["outcome"] = json!("committed");
            return Response::json(200, &receipt);
        }
        Err(error) => error,
    };
    let (status, name) = classify(&error.kind);
    if error.outcome == Outcome::Committed {
        return Response::json(
            200,
            &json!({"outcome": "committed", "receipt": null, "error": name}),
        );
    }
    let mut body = json!({"outcome": crate::outcome::name(error.outcome), "error": name});
    match error.kind {
        ErrorKind::ResourceExhausted(resource) => body["resource"] = json!(resource),
        ErrorKind::InvalidInput => body["message"] = json!(limits),
        _ => {}
    }
    Response::json(status, &body)
}

const LIMITS: &str = "turns must not be empty; content is at most 4 MiB, meta at most 64 KiB, \
                      agent and session at most 255 bytes each";
const FACT_LIMITS: &str = "key is 1 to 1024 bytes; content is at most 4 MiB, meta at most 64 KiB, \
                           agent at most 255 bytes";

fn classify(kind: &ErrorKind) -> (u16, &'static str) {
    match kind {
        ErrorKind::InvalidInput => (400, "invalid_input"),
        ErrorKind::IdempotencyConflict => (409, "idempotency_conflict"),
        ErrorKind::Conflict => (409, "conflict"),
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
    send(engine.window_budget(&scope, query, max_bytes), |window| {
        let turns: Vec<_> = window.turns.into_iter().map(turn_json).collect();
        json!({"truncated": window.truncated, "turns": turns})
    })
}

/// A read's answer. Meta is reparsed into trees, so the JSON is built under the tree
/// limit. The send budget is taken there too, so at most 2 built bodies wait for it.
fn send<T>(result: Result<T, EngineError>, build: impl FnOnce(T) -> Value) -> Response {
    let read = match result {
        Ok(read) => read,
        Err(error) => {
            let (status, name) = classify(&error.kind);
            return Response::json(status, &json!({"error": name}));
        }
    };
    let Some(_slot) = PARSERS.take(1, Duration::MAX) else { return busy() };
    let body = build(read);
    let response = Response::json(200, &body);
    drop(body);
    match SENDING.take(response.body.len(), SEND_WAIT) {
        Some(held) => Response { _held: Some(held), ..response },
        None => busy(),
    }
}

/// `GET /v1/facts/get?key=`, `GET /v1/facts/history?key=&max_bytes=` and
/// `GET /v1/facts/list?prefix=&after=&limit=&max_bytes=`.
pub fn fact_read(engine: &Engine, tenant: &str, route: Route, target: &str) -> Response {
    let answer = (|| {
        let (mut params, list) = (params(target)?, route == Route::FactList);
        let key = if list { None } else { params.remove("key") };
        let max_bytes = if route == Route::FactGet { None } else { params.remove("max_bytes") };
        let [prefix, after, limit] = match list {
            true => ["prefix", "after", "limit"].map(|name| params.remove(name)),
            false => [None, None, None],
        };
        if !params.is_empty() {
            return Err("unknown query parameter".to_owned());
        }
        let max_bytes = number(max_bytes, "max_bytes")?;
        let max_bytes =
            max_bytes.map_or(DEFAULT_WINDOW_BYTES, |bytes| cap(bytes, MAX_WINDOW_BYTES));
        let key = key.ok_or_else(|| "key is required".to_owned());
        Ok(match route {
            Route::FactGet => {
                send(engine.fact_get(tenant, &key?), |fact| json!({"fact": fact.map(fact_json)}))
            }
            Route::FactHistory => send(engine.fact_history(tenant, &key?, max_bytes), |history| {
                let revisions: Vec<_> = history.revisions.into_iter().map(revision_json).collect();
                json!({"revisions": revisions, "truncated": history.truncated})
            }),
            _ => {
                let limit = number(limit, "limit")?;
                let limit = limit.map_or(100, |limit| cap(limit, MAX_WINDOW_TURNS));
                let prefix = prefix.unwrap_or_default();
                if prefix.len() > 1024 || after.as_ref().is_some_and(|after| after.len() > 1024) {
                    return Err("prefix and after are at most 1024 bytes".into());
                }
                let query = FactQuery { prefix, after, limit, max_bytes };
                send(engine.fact_list(tenant, query), |page| {
                    let facts: Vec<_> = page.facts.into_iter().map(fact_json).collect();
                    json!({"facts": facts, "next": page.next})
                })
            }
        })
    })();
    answer.unwrap_or_else(|message| {
        Response::json(400, &json!({"error": "invalid_input", "message": message}))
    })
}

/// `GET /v1/search?q=&before=&limit=&max_bytes=`. Hits are newest first. Pass `next` as
/// `before` for the next page.
pub fn search(engine: &Engine, tenant: &str, target: &str) -> Response {
    let query = (|| {
        let mut params = params(target)?;
        let query = params.remove("q").filter(|q| !q.trim().is_empty());
        let query = query.ok_or("q is required")?;
        let before = number(params.remove("before"), "before")?;
        let limit = number(params.remove("limit"), "limit")?.map_or(10, |n| cap(n, 100)).max(1);
        let max_bytes = number(params.remove("max_bytes"), "max_bytes")?;
        let max_bytes = max_bytes.map_or(DEFAULT_WINDOW_BYTES, |n| cap(n, MAX_WINDOW_BYTES));
        if !params.is_empty() || query.len() > 1024 {
            return Err("unknown query parameter, or q over 1024 bytes".to_owned());
        }
        Ok(SearchQuery { query, before, limit, max_bytes, visits: SEARCH_VISITS })
    })();
    match query {
        Ok(query) => send(engine.search(tenant, query), |page| {
            let hits: Vec<_> = page.hits.into_iter().map(hit_json).collect();
            json!({"hits": hits, "next": page.next})
        }),
        Err(message) => Response::json(400, &json!({"error": "invalid_input", "message": message})),
    }
}

fn hit_json(hit: Hit) -> Value {
    match hit {
        Hit::Turn { agent, session, turn } => {
            let mut out = turn_json(turn);
            (out["kind"], out["agent"], out["session"]) =
                (json!("turn"), json!(agent), json!(session));
            out
        }
        Hit::Fact(fact) => {
            let mut out = fact_json(fact);
            out["kind"] = json!("fact");
            out
        }
    }
}

/// `GET /v1/sessions?limit=&after_agent=&after_session=`. Sessions with a published turn, in
/// byte order of agent, then session. Pass `next` as `after_agent` and `after_session`.
pub fn sessions(engine: &Engine, tenant: &str, target: &str) -> Response {
    let query = (|| {
        let mut params = params(target)?;
        let limit = number(params.remove("limit"), "limit")?;
        let limit = limit.map_or(100, |n| cap(n, MAX_WINDOW_TURNS)).max(1);
        let after = match (params.remove("after_agent"), params.remove("after_session")) {
            (Some(agent), Some(session)) => Some((agent, session)),
            (None, None) => None,
            _ => return Err("pass after_agent and after_session together".to_owned()),
        };
        if !params.is_empty() {
            return Err("unknown query parameter".to_owned());
        }
        Ok(SessionQuery { after, limit })
    })();
    match query {
        Ok(query) => send(engine.sessions(tenant, query), |page| {
            let sessions: Vec<_> = page
                .sessions
                .into_iter()
                .map(|s| json!({"agent": s.agent, "session": s.session, "turns": s.turns}))
                .collect();
            let next =
                page.next.map(|(agent, session)| json!({"agent": agent, "session": session}));
            let total = json!({"sessions": page.total_sessions, "turns": page.total_turns});
            json!({"sessions": sessions, "next": next, "total": total})
        }),
        Err(message) => Response::json(400, &json!({"error": "invalid_input", "message": message})),
    }
}

/// `GET /v1/records?before=&limit=&kind=&max_bytes=`. The tenant's turns and fact revisions,
/// newest first, each as written. Pass `next` as `before` for the next page.
pub fn records(engine: &Engine, tenant: &str, target: &str) -> Response {
    let query = (|| {
        let mut params = params(target)?;
        let before = number(params.remove("before"), "before")?;
        let limit = number(params.remove("limit"), "limit")?;
        let limit = limit.map_or(50, |n| cap(n, MAX_WINDOW_TURNS)).max(1);
        let kind = match params.remove("kind").as_deref() {
            None => None,
            Some("turn") => Some(RecordKind::Turn),
            Some("fact") => Some(RecordKind::Fact),
            Some(_) => return Err("kind must be turn or fact".to_owned()),
        };
        let max_bytes = number(params.remove("max_bytes"), "max_bytes")?;
        let max_bytes = max_bytes.map_or(DEFAULT_WINDOW_BYTES, |n| cap(n, MAX_WINDOW_BYTES));
        if !params.is_empty() {
            return Err("unknown query parameter".to_owned());
        }
        Ok(RecordQuery { before, limit, kind, max_bytes, visits: SEARCH_VISITS })
    })();
    match query {
        Ok(query) => send(engine.records(tenant, query), |page| {
            let records: Vec<_> = page.records.into_iter().map(record_json).collect();
            json!({"records": records, "next": page.next})
        }),
        Err(message) => Response::json(400, &json!({"error": "invalid_input", "message": message})),
    }
}

/// A turn as `search` shows it. A fact revision as `history` shows it, plus `kind` and `key`.
fn record_json(record: Record) -> Value {
    match record {
        Record::Turn { agent, session, turn } => hit_json(Hit::Turn { agent, session, turn }),
        Record::Fact { key, revision } => {
            let mut out = revision_json(revision);
            (out["kind"], out["key"]) = (json!("fact"), json!(key));
            out
        }
    }
}

fn parse_window(tenant: &str, target: &str) -> Result<(Scope, WindowQuery, usize), String> {
    let mut params = params(target)?;
    let mut take = |name: &str| params.remove(name);
    let (Some(agent), Some(session)) = (take("agent"), take("session")) else {
        return Err("agent and session are required".into());
    };
    let last = number(take("last"), "last")?;
    let since = number(take("since_seq"), "since_seq")?;
    let limit = number(take("limit"), "limit")?;
    let max_bytes = number(take("max_bytes"), "max_bytes")?;
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

/// The percent-decoded query parameters. A repeated name is an error.
fn params(target: &str) -> Result<BTreeMap<String, String>, String> {
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
    Ok(params)
}

fn number(value: Option<String>, name: &str) -> Result<Option<u64>, String> {
    let parse = |text: String| text.parse::<u64>().map_err(|_| format!("{name} must be a number"));
    value.map(parse).transpose()
}

fn cap(value: u64, max: usize) -> usize {
    usize::try_from(value).unwrap_or(max).min(max)
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

/// Adds `content` and `meta`. Content that is not UTF-8, only possible from direct Rust
/// API writes, comes back with U+FFFD and `"lossy": true`. Meta is the stored JSON object,
/// or `null`.
fn blobs(mut out: Value, content: Vec<u8>, meta: Vec<u8>) -> Value {
    let mut lossy = false;
    let mut text = |bytes: Vec<u8>| {
        String::from_utf8(bytes).unwrap_or_else(|error| {
            lossy = true;
            String::from_utf8_lossy(error.as_bytes()).into_owned()
        })
    };
    out["meta"] = match serde_json::from_slice(&meta) {
        _ if meta.is_empty() => Value::Null,
        Ok(meta) => meta,
        Err(_) => Value::String(text(meta)),
    };
    out["content"] = Value::String(text(content));
    if lossy {
        out["lossy"] = json!(true);
    }
    out
}

fn turn_json(turn: Turn) -> Value {
    let role = ROLES.iter().find(|(role, _)| *role == turn.role).map_or("", |(_, name)| *name);
    let (seq, lsn, commit_time_ns) = (turn.seq, turn.lsn, turn.commit_time_ns);
    let head = json!({"seq": seq, "lsn": lsn, "commit_time_ns": commit_time_ns, "role": role});
    blobs(head, turn.content, turn.meta)
}

fn fact_json(fact: StoredFact) -> Value {
    let StoredFact { key, lsn, commit_time_ns, agent, content, meta } = fact;
    blobs(
        json!({"key": key, "lsn": lsn, "commit_time_ns": commit_time_ns, "agent": agent}),
        content,
        meta,
    )
}

fn revision_json(revision: Revision) -> Value {
    let Revision { lsn, commit_time_ns, agent, deleted, content, meta } = revision;
    let head =
        json!({"lsn": lsn, "commit_time_ns": commit_time_ns, "agent": agent, "deleted": deleted});
    blobs(head, content, meta)
}
