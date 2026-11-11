//! `POST /mcp` on `serve`, over loopback TCP with raw HTTP.

use crate::support::{serve, sim_engine, Dir, Running, Via, TOKEN_A, TOKEN_B};
use atticdb::serve::MAX_CONNECTIONS;
use atticdb_tape::SimDisk;
use serde_json::{json, Value};
use std::collections::HashMap;

/// 1 answer: status, lowercase header names, body.
struct Answer {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.body))
    }

    fn session(&self) -> String {
        self.headers["mcp-session-id"].clone()
    }
}

/// A server and its TCP address.
struct Server {
    running: Running,
    _dir: Dir,
}

impl Server {
    fn start() -> Self {
        let dir = Dir::new();
        let running = serve(sim_engine(&SimDisk::new(4), |_| {}), &dir, MAX_CONNECTIONS);
        Self { running, _dir: dir }
    }

    /// 1 request over TCP on its own connection. `headers` are extra lines, such as
    /// `("Origin", ...)`.
    fn send(
        &self,
        method: &str,
        token: Option<&str>,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        send(&self.running.vias[0], method, token, headers, body)
    }

    /// A POST of `message` as `token`, with an optional session id.
    fn post(&self, token: &str, session: Option<&str>, message: &Value) -> Answer {
        let headers: Vec<_> = session.map(|id| ("Mcp-Session-Id", id)).into_iter().collect();
        self.send("POST", Some(token), &headers, &message.to_string())
    }

    /// `initialize` as `token`, then `notifications/initialized`. Returns the session id.
    fn session(&self, token: &str) -> String {
        self.session_as(token, "Hosted Test")
    }

    /// [`Self::session`] with `clientInfo.name` set to `name`.
    fn session_as(&self, token: &str, name: &str) -> String {
        let answer = self.post(token, None, &initialize(1, name));
        assert_eq!(answer.status, 200, "{}", answer.body);
        let id = answer.session();
        let ready = self.post(token, Some(&id), &notification());
        assert_eq!(ready.status, 202, "{}", ready.body);
        id
    }

    /// 1 tool call on session `id`. Returns the tool result.
    fn call(&self, token: &str, id: &str, tool: &str, args: Value) -> Value {
        let answer = self.post(token, Some(id), &call(7, tool, args));
        assert_eq!(answer.status, 200, "{tool}: {}", answer.body);
        answer.json()["result"].clone()
    }
}

/// 1 request to `/mcp` via `via`, on its own connection.
fn send(
    via: &Via,
    method: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: &str,
) -> Answer {
    let mut head = format!("{method} /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    let raw = via.raw(format!("{head}{body}").as_bytes());
    let (top, body) = raw.split_once("\r\n\r\n").unwrap_or_else(|| panic!("no head: {raw}"));
    let mut lines = top.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines
        .map(|line| line.split_once(':').unwrap())
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Answer { status, headers, body: body.to_owned() }
}

fn initialize(id: u64, name: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": name, "version": "1"}}})
}

fn notification() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

fn rpc(id: u64, method: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {}})
}

fn call(id: u64, tool: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
           "params": {"name": tool, "arguments": args}})
}

fn committed(result: &Value) -> bool {
    result["structuredContent"]["committed"] == true
}

#[test]
fn initialize_then_tools_over_post() {
    let server = Server::start();
    let answer = server.post(TOKEN_A, None, &initialize(1, "Hosted Test"));
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.headers["content-type"], "application/json");
    let id = answer.session();
    assert!(id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    assert_eq!(answer.json()["result"]["serverInfo"]["name"], "atticdb");

    let ready = server.post(TOKEN_A, Some(&id), &notification());
    assert_eq!((ready.status, ready.body.as_str()), (202, ""));
    assert_eq!(ready.headers["content-type"], "text/plain");

    let list = server.post(TOKEN_A, Some(&id), &rpc(2, "tools/list"));
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(list.json()["result"]["tools"].as_array().unwrap().len(), 7, "{}", list.body);

    let set = server.call(TOKEN_A, &id, "fact_set", json!({"key": "repo/x", "content": "one"}));
    assert!(committed(&set), "{set}");
    let got = server.call(TOKEN_A, &id, "fact_get", json!({"key": "repo/x"}));
    assert_eq!(got["content"][0]["text"], "one", "{got}");
    assert_eq!(got["structuredContent"]["fact"]["agent"], "hosted-test", "{got}");
}

#[test]
fn session_ids_are_required_and_per_tenant() {
    let server = Server::start();
    let id = server.session(TOKEN_A);

    let missing = server.post(TOKEN_A, None, &rpc(2, "tools/list"));
    assert_eq!(missing.status, 400, "{}", missing.body);
    let unknown = server.post(TOKEN_A, Some(&"0".repeat(64)), &rpc(2, "tools/list"));
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    // A notification without an id is not a new session.
    let ready = server.post(TOKEN_A, None, &notification());
    assert_eq!(ready.status, 400, "{}", ready.body);
    assert!(!ready.headers.contains_key("mcp-session-id"));

    // Tenant b with a's id runs nothing on either store.
    let set = call(3, "fact_set", json!({"key": "repo/b", "content": "from b"}));
    let other = server.post(TOKEN_B, Some(&id), &set);
    assert_eq!(other.status, 404, "{}", other.body);
    let b = server.session(TOKEN_B);
    let listed = server.call(TOKEN_B, &b, "fact_list", json!({"prefix": "repo/"}));
    assert_eq!(listed["structuredContent"]["facts"], json!([]), "{listed}");
    let listed = server.call(TOKEN_A, &id, "fact_list", json!({"prefix": "repo/"}));
    assert_eq!(listed["structuredContent"]["facts"], json!([]), "{listed}");
    // The id still works for its own tenant.
    let list = server.post(TOKEN_A, Some(&id), &rpc(4, "tools/list"));
    assert_eq!(list.status, 200, "{}", list.body);
}

#[test]
fn web_origins_are_refused() {
    let server = Server::start();
    let id = server.session(TOKEN_A);
    let set = call(2, "fact_set", json!({"key": "repo/o", "content": "evil"})).to_string();
    let headers = [("Mcp-Session-Id", id.as_str()), ("Origin", "https://evil.example")];
    let evil = server.send("POST", Some(TOKEN_A), &headers, &set);
    assert_eq!(evil.status, 403, "{}", evil.body);
    let listed = server.call(TOKEN_A, &id, "fact_list", json!({"prefix": "repo/"}));
    assert_eq!(listed["structuredContent"]["facts"], json!([]), "nothing ran: {listed}");

    let headers = [("Mcp-Session-Id", id.as_str()), ("Origin", "vscode-file://vscode-app")];
    let editor = server.send("POST", Some(TOKEN_A), &headers, &rpc(3, "tools/list").to_string());
    assert_eq!(editor.status, 200, "{}", editor.body);
}

#[test]
fn methods_versions_and_tokens() {
    let server = Server::start();
    for method in ["GET", "DELETE"] {
        assert_eq!(server.send(method, Some(TOKEN_A), &[], "").status, 405, "{method}");
    }
    let id = server.session(TOKEN_A);
    let list = rpc(2, "tools/list").to_string();
    for (version, status) in [("1999-01-01", 400), ("2025-11-25", 200), ("2024-11-05", 200)] {
        let headers = [("Mcp-Session-Id", id.as_str()), ("MCP-Protocol-Version", version)];
        let answer = server.send("POST", Some(TOKEN_A), &headers, &list);
        assert_eq!(answer.status, status, "{version}: {}", answer.body);
    }
    let headers = [("Mcp-Session-Id", id.as_str())];
    let unversioned = server.send("POST", Some(TOKEN_A), &headers, &list);
    assert_eq!(unversioned.status, 200, "no MCP-Protocol-Version: {}", unversioned.body);
    assert_eq!(server.send("POST", None, &headers, &list).status, 401);
}

#[test]
fn bodies_must_be_one_json_rpc_object() {
    let server = Server::start();
    let id = server.session(TOKEN_A);
    let batch = format!("[{}]", rpc(2, "tools/list"));
    for (body, code) in [("[]", -32600), (batch.as_str(), -32600), ("{", -32700)] {
        let answer = server.send("POST", Some(TOKEN_A), &[("Mcp-Session-Id", &id)], body);
        assert_eq!(answer.status, 400, "{body}: {}", answer.body);
        assert_eq!(answer.json()["error"]["code"], code, "{body}");
    }
}

#[test]
fn one_session_serves_concurrent_posts() {
    let server = Server::start();
    let id = server.session(TOKEN_A);
    std::thread::scope(|scope| {
        for thread in 0..2 {
            let (server, id) = (&server, &id);
            scope.spawn(move || {
                for i in 0..20 {
                    let args = json!({"key": format!("repo/t{thread}/k{i}"), "content": "v"});
                    let set = server.call(TOKEN_A, id, "fact_set", args);
                    assert!(committed(&set), "{set}");
                }
            });
        }
    });
    let other = server.session(TOKEN_A);
    let listed =
        server.call(TOKEN_A, &other, "fact_list", json!({"prefix": "repo/", "limit": 100}));
    let facts = listed["structuredContent"]["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 40, "{listed}");
}

#[test]
fn sessions_guard_fact_writes_and_name_the_agent() {
    let server = Server::start();
    let (one, two) = (server.session_as(TOKEN_A, "Visual Studio Code"), server.session(TOKEN_A));
    let set = |id: &str, content: &str| {
        server.call(TOKEN_A, id, "fact_set", json!({"key": "repo/k", "content": content}))
    };
    assert!(committed(&set(&one, "one")), "session 1 sets an absent key");
    let got = server.call(TOKEN_A, &two, "fact_get", json!({"key": "repo/k"}));
    assert_eq!(got["structuredContent"]["fact"]["agent"], "visual-studio-code", "{got}");
    assert!(committed(&set(&one, "one again")), "session 1 wrote the last value");
    let stale = set(&two, "two");
    assert_eq!(stale["isError"], true, "{stale}");
    let text = stale["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("conflict:"), "{text}");
    assert!(committed(&set(&two, "two")), "the retry after the conflict commits");
}

#[test]
fn file_tools_are_unknown() {
    let server = Server::start();
    let id = server.session(TOKEN_A);
    let file_ref = call(2, "file_ref", json!({"path": "/x", "about": "y"}));
    let answer = server.post(TOKEN_A, Some(&id), &file_ref);
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.json()["error"]["code"], -32602, "{}", answer.body);
}

#[cfg(unix)]
#[test]
fn mcp_answers_on_the_unix_socket() {
    let server = Server::start();
    let unix = &server.running.vias[1];
    assert!(matches!(unix, Via::Unix(_)));
    let init = initialize(1, "Hosted Test").to_string();
    let answer = send(unix, "POST", Some(TOKEN_A), &[], &init);
    assert_eq!(answer.status, 200, "{}", answer.body);
    let id = answer.session();
    let list = rpc(2, "tools/list").to_string();
    let listed = send(unix, "POST", Some(TOKEN_A), &[("Mcp-Session-Id", &id)], &list);
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(listed.json()["result"]["tools"].as_array().unwrap().len(), 7, "{}", listed.body);
}
