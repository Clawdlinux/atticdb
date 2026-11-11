//! `POST /mcp`: MCP over Streamable HTTP for agents with no local install. See design 0010.
//!
//! Each request carries 1 JSON-RPC message. `initialize` starts a session, and every other
//! message names it in `Mcp-Session-Id`. Sessions live in memory only, so a restart drops
//! them, and clients start again after the 404.

use crate::api;
use crate::http::{Budget, Head, Response};
use crate::mcp::{Hosted, VERSIONS};
use atticdb_core::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Most sessions per tenant.
pub const MAX_SESSIONS_PER_TENANT: usize = 64;
/// Most sessions in all.
pub const MAX_SESSIONS: usize = 4096;
/// A session idle this long is dropped.
pub const SESSION_IDLE: Duration = Duration::from_secs(3600);

/// `/mcp` message trees in memory at once, each held until its reply is built. It's apart
/// from the store routes' parse budget, which a tool call inside a session takes too.
static MCP_PARSERS: Budget = Budget::new(2);

/// Sessions by id, each with its tenant and last use. A full store drops the least recently
/// used session. Lookups scan at most [`MAX_SESSIONS`] entries.
pub struct Sessions<T> {
    per_tenant: usize,
    total: usize,
    idle: Duration,
    map: Mutex<HashMap<String, Entry<T>>>,
}

struct Entry<T> {
    tenant: String,
    used: Instant,
    value: Arc<Mutex<T>>,
}

impl<T> std::fmt::Debug for Sessions<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions").field("len", &self.lock().len()).finish_non_exhaustive()
    }
}

impl<T> Sessions<T> {
    /// An empty store with these caps.
    pub fn new(per_tenant: usize, total: usize, idle: Duration) -> Self {
        Self { per_tenant, total, idle, map: Mutex::default() }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry<T>>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores `value` for `tenant` under a new id: 32 random bytes in hex. Drops idle
    /// sessions first, then the least recently used one of a full tenant or a full store.
    pub fn insert(&self, tenant: &str, value: T, now: Instant) -> io::Result<String> {
        let id = crate::random_hex(32)?;
        let mut map = self.lock();
        map.retain(|_, entry| now.saturating_duration_since(entry.used) <= self.idle);
        if map.values().filter(|entry| entry.tenant == tenant).count() >= self.per_tenant {
            evict(&mut map, |entry| entry.tenant == tenant);
        }
        if map.len() >= self.total {
            evict(&mut map, |_| true);
        }
        let entry = Entry { tenant: tenant.to_owned(), used: now, value: Arc::new(value.into()) };
        map.insert(id.clone(), entry);
        Ok(id)
    }

    /// Session `id` of `tenant`. `None` when it's unknown, another tenant's, or idle past the
    /// limit. A hit refreshes its last use.
    pub fn get(&self, id: &str, tenant: &str, now: Instant) -> Option<Arc<Mutex<T>>> {
        let mut map = self.lock();
        let entry = map.get_mut(id).filter(|entry| entry.tenant == tenant)?;
        if now.saturating_duration_since(entry.used) > self.idle {
            map.remove(id);
            return None;
        }
        entry.used = entry.used.max(now);
        Some(entry.value.clone())
    }
}

/// Removes the least recently used entry that matches `pick`.
fn evict<T>(map: &mut HashMap<String, Entry<T>>, pick: impl Fn(&Entry<T>) -> bool) {
    let oldest = map.iter().filter(|(_, entry)| pick(entry)).min_by_key(|(_, entry)| entry.used);
    if let Some(id) = oldest.map(|(id, _)| id.clone()) {
        map.remove(&id);
    }
}

/// Answers 1 `POST /mcp` for `tenant`. The checks run in order: `Origin`,
/// `MCP-Protocol-Version`, the body, then the session.
pub fn mcp(
    sessions: &Sessions<Hosted>,
    engine: &Arc<Engine>,
    tenant: &str,
    head: &Head,
    body: &[u8],
) -> Response {
    // A DNS-rebinding page sends a web origin. Editors send none or their own scheme.
    let web = |origin: &[u8]| {
        let origin = origin.to_ascii_lowercase();
        origin.starts_with(b"http://") || origin.starts_with(b"https://")
    };
    if head.headers("origin").any(web) {
        return refuse(403, "web origins are not allowed");
    }
    if head
        .header("mcp-protocol-version")
        .is_some_and(|v| !VERSIONS.iter().any(|ok| ok.as_bytes() == v))
    {
        return refuse(400, "unsupported MCP-Protocol-Version");
    }
    let Some(_slot) = MCP_PARSERS.take(1, Duration::MAX) else { return api::busy() };
    let message = match envelope(body) {
        Ok(message) => message,
        Err(response) => return response,
    };
    let now = Instant::now();
    if message.get("method") == Some(&json!("initialize")) {
        let mut session = Hosted::new(engine.clone(), tenant);
        let reply = session.handle_value(message);
        if !reply.as_ref().is_some_and(|reply| reply.get("result").is_some()) {
            return answer(reply);
        }
        return match sessions.insert(tenant, session, now) {
            Ok(id) => Response { headers: vec![("Mcp-Session-Id", id)], ..answer(reply) },
            Err(error) => rpc_error(500, -32603, &crate::platform::no_random(error)),
        };
    }
    let Some(id) = head.header("mcp-session-id") else {
        return refuse(400, "missing Mcp-Session-Id");
    };
    let found = std::str::from_utf8(id).ok().and_then(|id| sessions.get(id, tenant, now));
    let Some(session) = found else {
        return rpc_error(404, -32001, "session not found");
    };
    // Calls on 1 session run one at a time. The store-wide lock is not held here.
    let reply = session.lock().unwrap_or_else(PoisonError::into_inner).handle_value(message);
    answer(reply)
}

/// `body` parsed once, if it's 1 JSON-RPC object. The caller holds an [`MCP_PARSERS`] slot.
fn envelope(body: &[u8]) -> Result<Value, Response> {
    match serde_json::from_slice::<Value>(body) {
        Err(_) => Err(rpc_error(400, -32700, "parse error")),
        Ok(message) if message.is_object() => Ok(message),
        Ok(_) => Err(rpc_error(400, -32600, "invalid request: send 1 JSON-RPC object")),
    }
}

/// 202 with no body for no reply. 400 for a parse or envelope error, else 200.
fn answer(reply: Option<Value>) -> Response {
    let Some(reply) = reply else {
        return Response::text(202, "text/plain", String::new());
    };
    let invalid = matches!(reply["error"]["code"].as_i64(), Some(-32700 | -32600));
    Response::json(if invalid { 400 } else { 200 }, &reply)
}

/// A transport-level refusal: nothing ran.
fn refuse(status: u16, message: &str) -> Response {
    rpc_error(status, -32000, message)
}

fn rpc_error(status: u16, code: i64, message: &str) -> Response {
    let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": code, "message": message}});
    Response::json(status, &body)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const IDLE: Duration = Duration::from_secs(60);

    fn hex(id: &str) -> bool {
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
    }

    fn value(sessions: &Sessions<u32>, id: &str, tenant: &str, now: Instant) -> Option<u32> {
        sessions.get(id, tenant, now).map(|value| *value.lock().unwrap())
    }

    #[test]
    fn a_full_tenant_drops_its_least_recently_used_session() {
        let (sessions, now) =
            (Sessions::new(MAX_SESSIONS_PER_TENANT, MAX_SESSIONS, IDLE), Instant::now());
        let other = sessions.insert("b", 99, now).unwrap();
        let ids: Vec<String> = (0..64)
            .map(|i| sessions.insert("a", i, now + Duration::from_millis(u64::from(i))).unwrap())
            .collect();
        assert!(ids.iter().all(|id| hex(id)), "{ids:?}");
        let later = now + Duration::from_secs(1);
        // Session 0 is used again, so session 1 is now the least recently used.
        assert_eq!(value(&sessions, &ids[0], "a", later), Some(0));
        let new = sessions.insert("a", 64, later).unwrap();
        assert_eq!(value(&sessions, &ids[1], "a", later), None, "evicted");
        assert_eq!(value(&sessions, &ids[0], "a", later), Some(0));
        assert_eq!(value(&sessions, &ids[2], "a", later), Some(2));
        assert_eq!(value(&sessions, &new, "a", later), Some(64));
        assert_eq!(value(&sessions, &other, "b", later), Some(99), "other tenants keep theirs");
    }

    #[test]
    fn the_total_cap_spans_tenants() {
        let (sessions, now) = (Sessions::new(64, 3, IDLE), Instant::now());
        let a = sessions.insert("a", 1, now).unwrap();
        let b = sessions.insert("b", 2, now + Duration::from_secs(1)).unwrap();
        let c = sessions.insert("c", 3, now + Duration::from_secs(2)).unwrap();
        let d = sessions.insert("d", 4, now + Duration::from_secs(3)).unwrap();
        let later = now + Duration::from_secs(4);
        assert_eq!(value(&sessions, &a, "a", later), None, "the oldest went");
        assert_eq!(value(&sessions, &b, "b", later), Some(2));
        assert_eq!(value(&sessions, &c, "c", later), Some(3));
        assert_eq!(value(&sessions, &d, "d", later), Some(4));
    }

    #[test]
    fn another_tenant_gets_nothing() {
        let (sessions, now) = (Sessions::new(64, 4096, IDLE), Instant::now());
        let id = sessions.insert("a", 1, now).unwrap();
        assert_eq!(value(&sessions, &id, "b", now), None);
        assert_eq!(value(&sessions, &id, "a", now), Some(1), "still there for its tenant");
        assert_eq!(value(&sessions, "unknown", "a", now), None);
    }

    #[test]
    fn idle_sessions_expire_and_a_get_refreshes() {
        let (sessions, now) = (Sessions::new(64, 4096, IDLE), Instant::now());
        let (kept, idle) =
            (sessions.insert("a", 1, now).unwrap(), sessions.insert("a", 2, now).unwrap());
        let half = now + IDLE / 2;
        assert_eq!(value(&sessions, &kept, "a", half), Some(1));
        let past = now + IDLE + Duration::from_secs(1);
        assert_eq!(value(&sessions, &idle, "a", past), None, "idle past the limit");
        assert_eq!(value(&sessions, &kept, "a", past), Some(1), "the get at half refreshed it");
        assert_eq!(value(&sessions, &kept, "a", past + IDLE + Duration::from_secs(1)), None);
    }
}
