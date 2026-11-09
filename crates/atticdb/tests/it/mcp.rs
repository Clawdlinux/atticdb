use crate::support::{
    self, chmod, init, init_config, kill_holder, private_file, text, wait_for, Dir, Holders, Mcp,
    Process, TOKEN_A,
};
use atticdb_tape::SimDisk;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn contents(result: &Value) -> Vec<&str> {
    let turns = result["structuredContent"]["turns"].as_array().unwrap();
    turns.iter().map(|turn| turn["content"].as_str().unwrap()).collect()
}

/// Checks a committed `memory_append` result. Returns its `seq`.
fn js_safe(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            n.as_i64().or(n.as_u64().map(|_| i64::MAX)).is_none_or(|n| n.abs() < 1 << 53)
        }
        Value::Array(items) => items.iter().all(js_safe),
        Value::Object(fields) => fields.values().all(js_safe),
        _ => true,
    }
}

fn contents_of_hits(result: &Value) -> Vec<&str> {
    let hits = result["structuredContent"]["hits"].as_array().unwrap();
    hits.iter().map(|hit| hit["content"].as_str().unwrap()).collect()
}

/// `text` with each `YYYY-MM-DD` date as `DATE`.
fn undated(text: &str) -> String {
    let is_date = |word: &str| {
        let digit =
            |(i, b): (usize, u8)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() };
        word.len() == 10 && word.bytes().enumerate().all(digit)
    };
    let (mut out, mut rest) = (String::new(), text);
    while let Some(c) = rest.chars().next() {
        if rest.get(..10).is_some_and(is_date) {
            out.push_str("DATE");
            rest = &rest[10..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// `tree --json` output without `newest_ns` and `lsn`, which change from run to run.
fn unstamped(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("newest_ns");
            fields.remove("lsn");
            fields.values_mut().for_each(unstamped);
        }
        Value::Array(items) => items.iter_mut().for_each(unstamped),
        _ => {}
    }
}

fn appended(result: &Value, case: &str) -> u64 {
    receipt(result, "seq", &["committed", "deduplicated", "lsn", "seq"], case)
}

/// Checks a committed `fact_set` result. Returns its `lsn`.
fn fact_written(result: &Value, case: &str) -> u64 {
    receipt(result, "lsn", &["committed", "deduplicated", "lsn"], case)
}

/// Checks a committed tool result with exactly the fields in `shape`. Returns field `id`.
fn receipt(result: &Value, id: &str, shape: &[&str], case: &str) -> u64 {
    let content = &result["structuredContent"];
    let n = content[id].as_u64().unwrap_or_else(|| panic!("{case}: {result}"));
    assert_eq!(text(result), format!("committed {id}={n}"), "{case}");
    assert!(content["lsn"].is_u64() && result["isError"].is_null(), "{case}");
    let keys: Vec<&String> = content.as_object().unwrap().keys().collect();
    assert_eq!(keys, shape, "{case}: receipt shape");
    n
}

/// A field of `data`'s `LOCK`.
fn lock_field(data: &Path, name: &str) -> String {
    let lock = std::fs::read_to_string(data.join("LOCK")).unwrap();
    lock.split(' ').find_map(|field| field.strip_prefix(name)).unwrap().to_owned()
}

/// What a fake listener saw.
#[derive(Clone, Default)]
struct Seen {
    connections: Arc<AtomicUsize>,
    authorized: Arc<AtomicUsize>,
}

/// A fake server: every request gets `/healthz` JSON with `instance`.
fn fake<S: Read + Write>(
    accept: impl Fn() -> std::io::Result<S> + Send + 'static,
    instance: &str,
) -> Seen {
    let (seen, body) = (Seen::default(), json!({"status": "ok", "instance": instance}).to_string());
    let record = seen.clone();
    std::thread::spawn(move || {
        while let Ok(mut stream) = accept() {
            record.connections.fetch_add(1, Ordering::SeqCst);
            let (mut head, mut byte) = (Vec::new(), [0]);
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            if String::from_utf8_lossy(&head).to_ascii_lowercase().contains("authorization:") {
                record.authorized.fetch_add(1, Ordering::SeqCst);
            }
            let length = body.len();
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n{body}");
        }
    });
    seen
}

fn fake_unix(socket: &Path, instance: &str) -> Seen {
    let listener = UnixListener::bind(socket).unwrap();
    fake(move || listener.accept().map(|(stream, _)| stream), instance)
}

fn lock_text(instance: &str, socket: &Path, tcp: &str) -> String {
    format!("pid=999999999 instance={instance} unix={} tcp={tcp}\n", socket.display())
}

/// Root-owned sockets that exist on stock macOS and Linux hosts. A test cannot create a
/// socket owned by another user without root, so it points `LOCK` at one of these.
const FOREIGN_SOCKETS: [&str; 5] = [
    "/run/systemd/notify",
    "/var/run/mDNSResponder",
    "/var/run/syslog",
    "/run/dbus/system_bus_socket",
    "/run/systemd/journal/stdout",
];

const FOREIGN_ROW: &str = "a socket owned by another user";

/// A root-owned socket from [`FOREIGN_SOCKETS`], when the tests do not run as root.
fn foreign_socket() -> Option<&'static Path> {
    let euid = rustix::process::geteuid().as_raw();
    FOREIGN_SOCKETS.iter().map(Path::new).find(|path| {
        std::fs::symlink_metadata(path)
            .is_ok_and(|meta| meta.file_type().is_socket() && meta.uid() != euid)
    })
}

/// Drives the adapter as a model's client would, against a server started by the test.
#[test]
fn tool_calls_over_stdio() {
    let dir = Dir::new();
    // 200 back-to-back connections can outrun reaping under a busy suite, so no lower cap.
    let cap = atticdb::serve::MAX_CONNECTIONS;
    let running = support::serve(support::sim_engine(&SimDisk::new(4), |_| {}), &dir, cap);
    let url = format!("http://{}", running.server.tcp_addr().unwrap());
    let args = ["--url", &url, "--token-file", &private_file(&dir, "t", TOKEN_A), "--agent", "ag"];
    let mut mcp = Mcp::start(&dir.0, &[&args[..], &["--session", "s1"]].concat(), None);

    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}), "ping before initialize");
    for method in ["server/discover", "tools/list"] {
        let reply = mcp.request(method, json!({}));
        assert_eq!(reply["error"]["code"], -32601, "{method} before initialize");
    }
    for (asked, answered) in [("2024-11-05", "2024-11-05"), ("2099-01-01", "2025-11-25")] {
        // The client reports roots, but `--session` is set, so none are asked for.
        let mut params = init(asked);
        params["capabilities"] = json!({"roots": {}});
        let reply = mcp.request("initialize", params);
        assert_eq!(reply["result"]["protocolVersion"], answered, "asked for {asked}");
        assert_eq!(reply["result"]["capabilities"], json!({"tools": {}}), "asked for {asked}");
    }
    mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let cancelled = json!({"requestId": 1, "reason": "test"});
    mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": cancelled}));
    // `response` checks ids, so a reply to either notification would fail here.
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}), "ping after initialize");

    // An invalid envelope runs no tool. The default-session window below shows no turn.
    for (case, envelope) in [
        ("no jsonrpc", json!({})),
        ("jsonrpc 1.0", json!({"jsonrpc": "1.0"})),
        ("object id", json!({"jsonrpc": "2.0", "id": {"a": 1}})),
        ("array id", json!({"jsonrpc": "2.0", "id": [1]})),
    ] {
        mcp.next_id += 1;
        let params = json!({"name": "memory_append", "arguments": {"content": "bad envelope"}});
        let mut message = json!({"id": mcp.next_id, "method": "tools/call", "params": params});
        message.as_object_mut().unwrap().extend(envelope.as_object().unwrap().clone());
        mcp.send(&message);
        let reply = mcp.response();
        let id = if envelope.get("id").is_some() { json!(null) } else { json!(mcp.next_id) };
        assert_eq!((&reply["error"]["code"], &reply["id"]), (&json!(-32600), &id), "{case}");
    }
    let tools = mcp.request("tools/list", json!({}))["result"]["tools"].clone();
    let names: Vec<&str> =
        tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    let five = ["memory_append", "memory_recent", "fact_set", "fact_get", "fact_list"];
    let rest = ["memory_search", "file_ref", "file_get", "fact_history"];
    assert_eq!(names, [&five[..], &rest[..]].concat());
    assert_eq!(tools[5]["inputSchema"]["required"], json!(["query"]));
    // VS Code runs read-only tools without a confirmation prompt.
    let read_only: Vec<bool> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["annotations"]["readOnlyHint"] == true)
        .collect();
    assert_eq!(read_only, [false, true, false, true, true, true, false, true, true]);
    let history = &tools[8]["inputSchema"];
    assert_eq!(history["required"], json!(["key"]));
    assert_eq!(history["properties"]["max_bytes"]["default"], 65536);
    assert_eq!(tools[6]["inputSchema"]["required"], json!(["path", "about"]));
    assert_eq!(tools[6]["annotations"]["destructiveHint"], false, "file_ref keeps versions");
    assert_eq!(tools[2]["annotations"]["destructiveHint"], true, "fact_set replaces and deletes");
    // Agents saved decisions without their reasons (handoff pilot, 2026-11-04).
    let described = |i: usize| tools[i]["description"].as_str().unwrap();
    assert!(described(0).contains("what changed and why, with its evidence"), "{}", described(0));
    assert!(described(2).contains("Put the reason and its evidence"), "{}", described(2));
    let refusal = "If the key changed since you last read it, or you haven't read it yet, \
                   nothing is written. You get its current value: merge and set again.";
    assert!(described(2).contains(refusal), "{}", described(2));
    let (set, get, list) =
        (&tools[2]["inputSchema"], &tools[3]["inputSchema"], &tools[4]["inputSchema"]);
    assert_eq!((&set["required"], &get["required"]), (&json!(["key"]), &json!(["key"])));
    for field in ["key", "content", "delete", "meta"] {
        assert!(set["properties"][field].is_object(), "fact_set {field}");
    }
    assert_eq!(list["required"], json!(["prefix"]));
    let limit = &list["properties"]["limit"];
    assert_eq!((&limit["default"], &limit["maximum"]), (&json!(50), &json!(200)));
    assert_eq!(list["properties"]["max_bytes"]["default"], 65536);
    let (append, recent) = (&tools[0]["inputSchema"], &tools[1]["inputSchema"]);
    assert_eq!(append["required"], json!(["content"]));
    for field in ["content", "role", "session", "meta"] {
        assert!(append["properties"][field].is_object(), "memory_append {field}");
    }
    assert_eq!(append["properties"]["role"]["default"], "assistant");
    assert_eq!(
        (&recent["properties"]["n"]["default"], &recent["properties"]["n"]["maximum"]),
        (&json!(20), &json!(200))
    );
    assert_eq!(recent["properties"]["max_bytes"]["default"], 65536);
    assert!(recent["properties"]["session"].is_object() && recent["required"].is_null());

    let one = mcp.call("memory_append", json!({"content": "one", "meta": {"k": 1}}));
    let first = appended(&one, "default session");
    let first_turn = first;
    assert_eq!(one["structuredContent"]["deduplicated"], false);
    let two = mcp.call("memory_append", json!({"content": "two", "role": "user", "session": "s2"}));
    appended(&two, "session argument");
    let three = mcp.call("memory_append", json!({"content": "three"}));
    assert_eq!(appended(&three, "second append"), first + 1, "seqs follow in a session");

    let window = mcp.call("memory_recent", json!({"n": 1}));
    assert_eq!(contents(&window), ["three"], "n keeps the newest");
    let window = mcp.call("memory_recent", json!({}));
    assert_eq!(contents(&window), ["one", "three"], "default session, oldest first");
    assert_eq!(text(&window), format!("#{first} assistant: one\n#{} assistant: three", first + 1));
    assert_eq!(window["structuredContent"]["turns"][0]["meta"], json!({"k": 1}));
    let window = mcp.call("memory_recent", json!({"session": "s2", "max_bytes": 3}));
    assert_eq!(contents(&window), ["two"], "session argument");
    let window = mcp.call("memory_recent", json!({"session": "s2", "max_bytes": 2}));
    assert_eq!(
        (contents(&window).len(), &window["structuredContent"]["truncated"]),
        (0, &json!(true))
    );
    // The adapter writes as `--agent` inside the token's tenant, whatever the session.
    let (_, server) = running.vias[1].a("GET", "/v1/window?agent=ag&session=s2&last=10", None);
    assert_eq!(server["turns"][0]["content"], "two");

    let first = mcp.call("fact_set", json!({"key": "repo/a", "content": "one"}));
    let lsn = fact_written(&first, "first set");
    assert_eq!(first["structuredContent"]["deduplicated"], false);
    let second = mcp.call("fact_set", json!({"key": "repo/a", "content": "two", "meta": {"k": 2}}));
    assert!(fact_written(&second, "second set") > lsn);
    let got = mcp.call("fact_get", json!({"key": "repo/a"}));
    let fact = &got["structuredContent"]["fact"];
    assert_eq!(
        (text(&got), &fact["content"], &fact["meta"]),
        ("two", &json!("two"), &json!({"k": 2}))
    );
    assert_eq!(fact["agent"], "ag", "fact_set writes as --agent");
    fact_written(&mcp.call("fact_set", json!({"key": "repo/a", "delete": true})), "delete");
    let got = mcp.call("fact_get", json!({"key": "repo/a"}));
    assert_eq!(
        (text(&got), &got["structuredContent"]),
        ("no current value", &json!({"fact": null}))
    );
    for key in ["repo/b", "repo/c"] {
        fact_written(&mcp.call("fact_set", json!({"key": key, "content": key})), key);
    }
    let page = mcp.call("fact_list", json!({"prefix": "repo/", "limit": 1}));
    assert_eq!(text(&page), "repo/b: repo/b\nmore: call again with after=repo/b");
    let mut fields: Vec<&str> =
        page["structuredContent"].as_object().unwrap().keys().map(String::as_str).collect();
    fields.sort();
    assert_eq!(
        (fields, &page["structuredContent"]["next"]),
        (vec!["facts", "next"], &json!("repo/b"))
    );
    let page = mcp.call("fact_list", json!({"prefix": "repo/", "after": "repo/b"}));
    assert_eq!((text(&page), &page["structuredContent"]["next"]), ("repo/c: repo/c", &Value::Null));
    // Search spans sessions and current facts, newest first; a deleted fact is gone.
    let found = mcp.call("memory_search", json!({"query": "T"}));
    let (found, seq) = (text(&found), first_turn + 1);
    assert_eq!(found, format!("[ag/s1 #{seq} assistant] three\n[ag/s2 #1 user] two"));
    assert_eq!(
        text(&mcp.call("memory_search", json!({"query": "REPO/"}))),
        "[fact repo/c] repo/c\n[fact repo/b] repo/b"
    );
    let page = mcp.call("memory_search", json!({"query": "t", "limit": 1}));
    let before = page["structuredContent"]["next"].as_u64().unwrap();
    assert!(text(&page).ends_with(&format!("more: call again with before={before}")));
    let rest = mcp.call("memory_search", json!({"query": "t", "before": before}));
    assert_eq!(contents_of_hits(&rest), ["two"]);
    // 2 agents share session s1, so each heading names its agent.
    let other = support::append_body("s1", "z1", "zebra one");
    assert_eq!(running.vias[1].a("POST", "/v1/append", Some(&other)).0, 200);
    appended(&mcp.call("memory_append", json!({"content": "zebra two"})), "zebra");
    let (found, seq) = (mcp.call("memory_search", json!({"query": "zebra"})), first_turn + 2);
    assert_eq!(
        text(&found),
        format!("[ag/s1 #{seq} assistant] zebra two\n[a/s1 #1 user] zebra one")
    );
    // A model may fill an optional field with "". An empty session is the default one, s1.
    let empty = mcp.call("memory_append", json!({"content": "no name", "session": ""}));
    assert_eq!(appended(&empty, "empty session"), first_turn + 3, "the default session");
    let window = mcp.call("memory_recent", json!({"n": 1, "session": ""}));
    assert_eq!(contents(&window), ["no name"], "memory_recent reads the default session too");
    let (_, unnamed) = running.vias[1].a("GET", "/v1/window?agent=ag&session=&last=9", None);
    assert_eq!(unnamed["turns"], json!([]), "nothing in session \"\"");
    fact_written(&mcp.call("fact_set", json!({"key": "e", "content": ""})), "empty");
    assert_eq!(text(&mcp.call("fact_get", json!({"key": "e"}))), "(empty)");
    // Keys count toward `max_bytes`, so a page of escaped NULs fits the response cap.
    let nul = |n| "\0".repeat(n);
    for i in 0..200 {
        let body = json!({"key": format!("n/{}{i:04}", nul(1018)), "content": nul(41_900),
                          "idem_key": format!("n{i}")});
        assert_eq!(running.vias[1].a("POST", "/v1/facts/put", Some(&body)).0, 200);
    }
    let query = json!({"prefix": "n/", "limit": 200, "max_bytes": 8 << 20});
    let page = &mcp.call("fact_list", query)["structuredContent"];
    let (count, next) = (page["facts"].as_array().map(Vec::len), format!("n/{}0194", nul(1018)));
    assert!(count == Some(195) && page["next"] == json!(next), "{count:?}");
    let one = "pass exactly one of content or delete: true";
    for (case, args, expect) in [
        ("content and delete", json!({"key": "k", "content": "x", "delete": true}), one),
        ("neither", json!({"key": "k"}), one),
        (
            "delete with meta",
            json!({"key": "k", "delete": true, "meta": {}}),
            "meta must be an object, and only with content",
        ),
    ] {
        let rejected = mcp.call("fact_set", args);
        assert_eq!((rejected["isError"].clone(), text(&rejected)), (json!(true), expect), "{case}");
    }

    let rejected = mcp.call("memory_append", json!({"content": "x", "role": "robot"}));
    assert_eq!(rejected["isError"], true);
    let expect = "not_committed: nothing was written (invalid_input: role must be";
    assert!(text(&rejected).starts_with(expect), "{rejected}");
    let missing = mcp.call("memory_append", json!({"role": "user"}));
    assert_eq!((missing["isError"].clone(), text(&missing)), (json!(true), "content is required"));
    // No tool forgets (Q1): the user runs `atticdb forget` or uses studio.
    for name in ["memory_delete", "memory_forget"] {
        let reply = mcp.request("tools/call", json!({"name": name, "arguments": {}}));
        assert_eq!(reply["error"]["code"], -32602, "unknown tool {name}");
    }

    // Tokens in the content and in nested meta are stored as placeholders, and counted.
    // They are built here, as a token-shaped literal would trip push protection.
    let github = format!("ghp_{}", "x1".repeat(18));
    let (aws, slack) = (format!("AKIA{}", "AB12".repeat(4)), format!("xoxb-{}", "12345-abcd"));
    let meta = json!({"nested": {"key": aws}, "list": ["kept", slack], "n": 1});
    let note = json!({"content": format!("push with {github}"), "session": "r", "meta": meta});
    let saved = mcp.call("memory_append", note);
    let counts = json!({"aws-key-id": 1, "github-token": 1, "slack-token": 1});
    let content = &saved["structuredContent"];
    assert_eq!((&content["committed"], &content["redacted"]), (&json!(true), &counts), "{saved}");
    assert!(text(&saved).ends_with(&format!("; redacted {counts}")), "{saved}");
    let turn =
        mcp.call("memory_recent", json!({"session": "r"}))["structuredContent"]["turns"][0].clone();
    assert_eq!(turn["content"], "push with [secret:github-token]");
    let meta = json!({"nested": {"key": "[secret:aws-key-id]"},
        "list": ["kept", "[secret:slack-token]"], "n": 1});
    assert_eq!(turn["meta"], meta, "meta strings hidden, keys and numbers kept");
    let stored = turn.to_string();
    assert!(![&github, &aws, &slack].iter().any(|token| stored.contains(*token)), "{stored}");

    // A line over 8 MiB is refused, and the next line is read as usual.
    mcp.next_id += 1;
    let long =
        json!({"jsonrpc": "2.0", "id": mcp.next_id, "method": "ping", "pad": "x".repeat(8 << 20)});
    mcp.send(&long);
    let refused = json!({"code": -32600, "message": "message longer than 8 MiB"});
    assert_eq!(mcp.response(), json!({"jsonrpc": "2.0", "id": null, "error": refused}));
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}), "ping after a long line");

    // A client with roots and no `--session`: the session is the repo of the first file root.
    let app = dir.0.join("my 100% app");
    std::fs::create_dir_all(app.join(".git")).unwrap();
    let mut rooted = Mcp::start(&dir.0, &args, None);
    let capabilities = json!({"roots": {"listChanged": true}});
    let params = json!({"protocolVersion": "2025-11-25", "capabilities": capabilities});
    assert!(rooted.request("initialize", params)["result"].is_object());
    rooted.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let ask = rooted.asked();
    assert_eq!((&ask["method"], &ask["id"]), (&json!("roots/list"), &json!("atticdb-roots-1")));
    let src = app.join("src").to_str().unwrap().replace('%', "%25").replace(' ', "%20");
    let roots = json!([{"uri": "https://example.com/"}, {"uri": format!("file://{src}")}]);
    // A call without `session` sent before the answer waits for it. A ping behind it waits too.
    rooted.post_call("memory_append", json!({"content": "rooted"}));
    rooted.post("ping", json!({}));
    rooted.send(&json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"roots": roots}}));
    appended(&rooted.response()["result"], "rooted");
    assert_eq!(rooted.response()["result"], json!({}), "the ping is answered after the call");
    let window = rooted.call("memory_recent", json!({"session": "my 100% app"}));
    assert_eq!(contents(&window), ["rooted"], "session from the root");
    rooted.send(&json!({"jsonrpc": "2.0", "method": "notifications/roots/list_changed"}));
    assert_eq!(rooted.asked()["id"], "atticdb-roots-2", "asked again on a change");
    assert_eq!(rooted.close().0.code(), Some(0));

    // A client that never answers roots: only the first default-session call waits 2 s.
    // An append alone can take hundreds of ms on a CI runner's disk. So the windows only
    // prove 1 wait for the first call, as 2 would pass 3.9 s, and none for the second.
    let mut silent = Mcp::start(&dir.0, &args, None);
    let params = json!({"protocolVersion": "2025-11-25", "capabilities": {"roots": {}}});
    assert!(silent.request("initialize", params)["result"].is_object());
    silent.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    assert_eq!(silent.asked()["method"], "roots/list");
    // The first call's empty session is the default session too, so it waits as well.
    let rows = [("slow", json!(""), 1_900..3_900), ("fast", Value::Null, 0..1_900)];
    for (content, session, waits) in rows {
        let started = std::time::Instant::now();
        let args = json!({"content": content, "session": session});
        appended(&silent.call("memory_append", args), content);
        let elapsed = started.elapsed().as_millis() as u64;
        assert!(waits.contains(&elapsed), "{content}: {elapsed} ms");
    }
    let session = atticdb::home::repo(&dir.0).unwrap();
    let window = silent.call("memory_recent", json!({"session": session}));
    assert_eq!(contents(&window), ["slow", "fast"], "both in the working folder's repo");
    assert_eq!(silent.close().0.code(), Some(0));

    let (status, _) = mcp.close();
    assert_eq!(status.code(), Some(0), "stdin closed: {}", mcp.process.stderr());
    let stdout = mcp.process.stdout();
    for line in stdout.lines() {
        let message: Value = serde_json::from_str(line).unwrap();
        assert_eq!(message["jsonrpc"], "2.0", "stdout holds only JSON-RPC: {line}");
        // JavaScript clients fail on integers past 2^53: commit_time_ns must arrive as text.
        assert!(js_safe(&message), "integer past 2^53 in: {line}");
    }
    assert!(stdout.contains("\"commit_time_ns\":\""), "commit_time_ns kept, as a string");
    let hex_run = stdout.as_bytes().windows(32).any(|w| w.iter().all(u8::is_ascii_hexdigit));
    let hidden = !hex_run && !stdout.contains("idem") && !stdout.contains("if_lsn");
    assert!(hidden, "no retry key or condition reaches the model: {stdout}");
    assert!(!stdout.contains(TOKEN_A) && !stdout.contains("tenant"), "{stdout}");
    assert!(
        !stdout.contains("roots/list"),
        "--session: no roots asked, though the client has them"
    );

    // The same tools from a shell: one JSON line on stdout, the reason on stderr.
    let cli = |verb: &[&str], code: i32| {
        let mut process = support::Process::spawn(&dir.0, &[verb, &args[..]].concat());
        let (status, out) = process.wait();
        assert_eq!(status.code(), Some(code), "{verb:?}: {out}");
        (process.stdout(), process.stderr())
    };
    // (command, a JSON pointer into what it prints, the value there)
    for (verb, pointer, expected) in [
        (&["append", "--session", "sh", "--content", "hi", "--role", "user"][..], "/seq", json!(1)),
        (&["recent", "--session", "sh", "--n", "1"], "/turns/0/content", json!("hi")),
        (&["fact", "set", "--key", "sh/k", "--content", "v"], "/committed", json!(true)),
        (&["fact", "get", "--key", "sh/k"], "/fact/content", json!("v")),
        (&["fact", "list", "--prefix", "sh/", "--limit", "5"], "/facts/0/key", json!("sh/k")),
        (&["fact", "set", "--key", "sh/k", "--delete"], "/committed", json!(true)),
        (&["fact", "get", "--key", "sh/k"], "", json!({"fact": null})),
        (&["search", "--query", "HI", "--limit", "1"], "/hits/0/session", json!("sh")),
    ] {
        let (out, err) = cli(verb, 0);
        let printed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(printed.pointer(pointer), Some(&expected), "{verb:?}: {out}");
        assert!(!err.contains("No hits."), "{verb:?}: {err}");
    }
    // `atticdb forget` prints 3 lines: the receipt, what a forget hides, and what stays visible.
    // Only `forget session` takes --agent and --session, so these get the store flags alone.
    let forget = |verb: &[&str]| {
        let mut process =
            support::Process::spawn(&dir.0, &[&["forget"], verb, &args[..4]].concat());
        let (status, out) = process.wait();
        assert_eq!(status.code(), Some(0), "{verb:?}: {out}");
        process.stdout()
    };
    let recent = serde_json::from_str::<Value>(&cli(&["recent", "--session", "sh"], 0).0).unwrap();
    let hi = recent["turns"][0]["lsn"].as_u64().unwrap();
    let (gone, hi_text) = (std::fs::canonicalize(&dir.0).unwrap().join("gone.png"), hi.to_string());
    let hides =
        "Content is hidden from normal AtticDB reads. It stays on disk until purge ships (#159).";
    let visible = "Names, keys, LSNs, counts and sizes stay visible. See \"What a forget leaves \
                   visible\" in the README.";
    let mut previous = 0;
    // (verb and its flags, how line 1 names the selector, up_to_lsn)
    for (verb, named, up_to) in [
        (&["lsns", "--lsns", &format!("{hi},{hi}")][..], "1 lsn".to_owned(), None),
        (
            &["session", "--agent", "ag", "--session", "sh", "--up-to-lsn", &hi_text],
            "session ag/sh".into(),
            Some(hi),
        ),
        (&["key", "--key", "sh/k"], "key sh/k".into(), None),
        // The ref of a file that is gone is still forgotten by its path.
        (&["file", "--path", gone.to_str().unwrap()], format!("file {}", gone.display()), None),
    ] {
        let out = forget(verb);
        let lines: Vec<&str> = out.lines().collect();
        let numbers: Vec<u64> =
            lines[0].split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect();
        let [cutoff, txn, lsn] = numbers[numbers.len() - 3..] else { panic!("{verb:?}: {out}") };
        assert_eq!(
            lines[0],
            format!("forgot {named} up to lsn {cutoff} (txn {txn}, lsn {lsn})"),
            "{verb:?}"
        );
        assert_eq!(
            cutoff,
            up_to.unwrap_or(lsn),
            "{verb:?}: the cutoff is up_to_lsn, else the forget"
        );
        assert!(lsn > previous, "{verb:?}: 1 durable record each");
        previous = lsn;
        assert_eq!(lines[1..], [hides, visible], "{verb:?}");
    }
    // A forgotten turn reads as its seq and `(forgotten)`, with no content.
    let mut reader = Mcp::start(&dir.0, &[&args[..], &["--session", "sh"]].concat(), None);
    reader.handshake();
    let window = reader.call("memory_recent", json!({}));
    assert_eq!(text(&window), "#1 (forgotten)", "{window}");
    assert_eq!(
        window["structuredContent"]["turns"],
        json!([{"seq": 1, "lsn": hi, "redacted": true}])
    );
    assert_eq!(reader.close().0.code(), Some(0));
    // `atticdb tree` draws the keys under a prefix, with counts and dates, never a forgotten
    // key. It reads only through `fact_list`.
    let long = format!("{}\nline 2", "a".repeat(100));
    for (key, content) in [
        ("tree/user/style", long.as_str()),
        ("tree/repo/app/api/retry", "Retry 3 times."),
        ("tree/repo/app/api/timeout", "30 s"),
        ("tree/repo/app/tests/runner", "cargo test"),
        ("tree/repo/other/x", "x"),
        ("tree/agent/claude/notes", "secret plan"),
        ("odd/x/", "a key that ends in /"),
    ] {
        cli(&["fact", "set", "--key", key, "--content", content], 0);
    }
    forget(&["key", "--key", "tree/agent/claude/notes"]);
    let nested = "tree/  5 facts, newest DATE\n\
        |-- repo/  4 facts, newest DATE\n\
        |   |-- app/  3 facts, newest DATE\n\
        |   |   |-- api/  2 facts, newest DATE\n\
        |   |   |   |-- retry  DATE\n\
        |   |   |   `-- timeout  DATE\n\
        |   |   `-- tests/  1 fact, newest DATE\n\
        |   |       `-- runner  DATE\n\
        |   `-- other/  1 fact, newest DATE\n\
        |       `-- x  DATE\n\
        `-- user/  1 fact, newest DATE\n    \
            `-- style  DATE\n";
    let depth = "tree/  5 facts, newest DATE\n\
        |-- repo/  4 facts, newest DATE\n\
        |   |-- app/  3 facts, newest DATE\n\
        |   `-- other/  1 fact, newest DATE\n\
        `-- user/  1 fact, newest DATE\n    \
            `-- style  DATE\n\
        Deeper levels show as counts. Pass --depth <n> to see more.\n";
    let content =
        format!("tree/user/  1 fact, newest DATE\n`-- style  DATE  {}...\n", "a".repeat(77));
    // (case, command, what stdout says with dates as DATE)
    for (case, verb, expected) in [
        ("nested tree", &["tree", "tree/"][..], nested.to_owned()),
        (
            "prefix filter",
            &["tree", "tree/repo/app/api/"],
            "tree/repo/app/api/  2 facts, newest DATE\n|-- retry  DATE\n`-- timeout  DATE\n".into(),
        ),
        (
            "prefix inside a segment",
            &["tree", "tree/repo/o"],
            "tree/repo/o  1 fact, newest DATE\n`-- other/  1 fact, newest DATE\n    `-- x  DATE\n"
                .into(),
        ),
        ("--depth", &["tree", "tree/", "--depth", "2"], depth.to_owned()),
        ("--content cuts the first line", &["tree", "tree/user/", "--content"], content),
        (
            "a key that ends in /",
            &["tree", "odd/"],
            "odd/  1 fact, newest DATE\n`-- x/  1 fact, newest DATE\n    `-- (empty)  DATE\n"
                .into(),
        ),
    ] {
        let (out, err) = cli(verb, 0);
        assert_eq!(undated(&out), expected, "{case}");
        assert!(err.is_empty(), "{case}: {err}");
    }
    // In JSON the empty name stays "".
    let (out, _) = cli(&["tree", "odd/x/", "--json"], 0);
    let printed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(printed["nodes"][0]["name"], "", "{out}");
    // The only key under tree/agent/ is forgotten, so the tree is empty and says how to save
    // one. It names this atticdb by its full path when it isn't on PATH, and repeats the
    // store flags, so the fact lands in the store the tree read.
    let (empty, save) = ("No facts under \"tree/agent/\". Save one: ", " fact set --url ");
    let (out, _) = cli(&["tree", "tree/agent/"], 0);
    let typed = out.strip_prefix(empty).and_then(|rest| rest.split_once(save)).map(|s| s.0);
    assert!(typed.is_some_and(|typed| typed.contains("atticdb")), "{out}");
    let store = format!("--url {} --token-file {}", args[1], args[3]);
    let saved = format!(" fact set {store} --key tree/agent/rule --content \"...\"\n");
    assert!(out.ends_with(&saved), "{out}");
    // `--json` prints 1 line of nested nodes. A folder past `--depth` has null children.
    let (out, _) = cli(&["tree", "tree/repo/", "--json", "--depth", "2", "--content"], 0);
    let mut printed: Value = serde_json::from_str(&out).unwrap();
    let newest = printed["newest_ns"].as_u64().unwrap();
    let (app, x) = (&printed["nodes"][0], &printed["nodes"][1]["children"][0]);
    assert!(app["newest_ns"].as_u64() <= Some(newest) && app["newest_ns"].as_u64() > Some(0));
    assert!(x["lsn"].as_u64() > Some(0) && x["children"].is_null(), "a key: {x}");
    unstamped(&mut printed);
    let expected = json!({"prefix": "tree/repo/", "facts": 4, "truncated": false, "nodes": [
        {"name": "app/", "path": "tree/repo/app/", "facts": 3, "children": [
            {"name": "api/", "path": "tree/repo/app/api/", "facts": 2, "children": null},
            {"name": "tests/", "path": "tree/repo/app/tests/", "facts": 1, "children": null},
        ]},
        {"name": "other/", "path": "tree/repo/other/", "facts": 1, "children": [
            {"name": "x", "path": "tree/repo/other/x", "facts": 1, "content": "x"},
        ]},
    ]});
    assert_eq!(printed, expected, "--json shape");
    // With no facts, `--json` prints an empty tree and the hint goes to stderr.
    let (out, err) = cli(&["tree", "tree/agent/", "--json", "--content"], 0);
    let printed: Value = serde_json::from_str(&out).unwrap();
    let none = json!({"prefix": "tree/agent/", "facts": 0, "newest_ns": null, "truncated": false,
                      "nodes": []});
    assert_eq!(printed, none, "empty --json");
    assert!(err.starts_with(empty) && err.contains(&format!("{store} --key tree/agent/rule")));
    let (all, _) = cli(&["tree", "tree/", "--content", "--json"], 0);
    assert!(!all.contains("agent") && !all.contains("secret plan"), "forgotten: {all}");
    // A search that finds nothing prints the same JSON, and a hint on stderr.
    let (out, err) = cli(&["search", "--query", "hi there"], 0);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!({"hits": [], "next": null}));
    assert!(err.contains("No hits. Search matches every word. Try 1 or 2 words.\n"), "{err}");
    // (command, exit code, what stderr says)
    for (verb, code, reason) in [
        (&["fact", "set", "--key", "k", "--content", "x", "--delete"][..], 1, "pass exactly one"),
        (&["append", "--content", "x", "--role", "robot"], 1, "not_committed: nothing was written"),
        (&["recent", "--n", "x"], 2, ""),
        (&["fact", "drop", "--key", "k"], 2, ""),
        (&["recent", "--key", "k"], 2, ""),
        // `cli` passes --agent but no --session.
        (&["forget", "session"], 2, "forget session needs --session"),
        (&["forget", "key", "--key", "k"], 2, "atticdb forget key <connect>"),
        (&["tree", "--depth", "x"], 2, "--depth must be a whole number"),
        (&["tree", "a/", "b/"], 2, "atticdb tree [<prefix>] <connect>"),
    ] {
        let (out, err) = cli(verb, code);
        assert!(out.is_empty() && err.contains(reason), "{verb:?}: {out} {err}");
    }
    // Every attempt is sent and then cut off, so the outcome stays unknown.
    let lossy = TcpListener::bind("127.0.0.1:0").unwrap();
    let lossy_url = format!("http://{}", lossy.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut conn in lossy.incoming().map_while(Result::ok) {
            let _ = conn.read(&mut [0; 4096]);
        }
    });
    let unknown = ["append", "--url", &lossy_url, "--content", "x", "--retry-deadline", "2500ms"];
    let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
    command.args(unknown).current_dir(&dir.0).env("ATTICDB_TOKEN", TOKEN_A);
    let (status, out) = support::Process::start(command.stdin(std::process::Stdio::null())).wait();
    assert_eq!(status.code(), Some(3), "unknown outcome: {out}");
    assert!(out.contains("atticdb: unknown: the write may be saved"), "{out}");

    // No attempt sent a byte, so nothing was written. A listener whose backlog is full
    // never accepts: connecting must still stop at the deadline.
    let _full = support::saturated(&dir.0.join("full"));
    // FIFOs where a file belongs must not block an open: the call fails by its deadline.
    init_config(&dir);
    for name in ["fifo-log", "fifo-lock"] {
        std::fs::DirBuilder::new().mode(0o700).create(dir.0.join(name)).unwrap();
    }
    support::mkfifo(&dir.0.join("fifo-log/serve.log"));
    support::mkfifo(&dir.0.join("fifo-lock/LOCK"));
    let (gone, full) = (dir.0.join("gone"), format!("unix://{}", dir.0.join("full").display()));
    let gone = format!("unix://{}", gone.display());
    let rows: [(&str, &[&str], &str, &str); 6] = [
        ("no socket", &["--url", &gone], "2500ms", ""),
        ("no LOCK", &["--data-dir", "empty"], "2500ms", ""),
        ("full backlog", &["--url", &full], "2500ms", ""),
        ("server up, deadline under 2 s", &["--url", &url], "1s", "less than 2 s left"),
        (
            "FIFO at serve.log on auto-start",
            &["--config", "c.json", "--data-dir", "fifo-log"],
            "2500ms",
            "",
        ),
        ("FIFO at LOCK during discovery", &["--data-dir", "fifo-lock"], "2500ms", ""),
    ];
    for (case, args, deadline, detail) in rows {
        let args = [args, &["--retry-deadline", deadline, "--session", "lost"]].concat();
        let mut mcp = Mcp::start(&dir.0, &args, Some(TOKEN_A));
        mcp.handshake();
        let result = mcp.call("memory_append", json!({"content": "lost?"}));
        assert_eq!(result["isError"], true, "{case}");
        let expect = format!("not_committed: nothing was written ({detail}");
        assert!(text(&result).starts_with(&expect), "{case}: {result}");
        assert_eq!(mcp.close().0.code(), Some(0));
    }
    // A start that cannot succeed fails at once with its reason, not at the deadline.
    std::fs::DirBuilder::new().mode(0o700).create(dir.0.join("open")).unwrap();
    chmod(&dir.0.join("open"), 0o755);
    std::fs::DirBuilder::new().mode(0o700).create(dir.0.join("bad")).unwrap();
    std::fs::write(dir.0.join("bad/SUPERBLOCK"), [0xAB; 24]).unwrap();
    for (data, reason) in [("open", "must be a directory owned by"), ("bad", "exited")] {
        let args = ["--config", "c.json", "--data-dir", data, "--retry-deadline", "30s"];
        let started = std::time::Instant::now();
        let mut mcp = Mcp::start(&dir.0, &args, Some(TOKEN_A));
        mcp.handshake();
        let result = mcp.call("memory_append", json!({"content": "lost?"}));
        assert!(text(&result).contains(reason), "{data}: {result}");
        assert!(started.elapsed().as_secs() < 5, "{data}: took {:?}", started.elapsed());
        assert_eq!(mcp.close().0.code(), Some(0));
    }
    let (_, lost) = running.vias[1].a("GET", "/v1/window?agent=mcp&session=lost&last=9", None);
    assert_eq!(lost["turns"], json!([]), "nothing sent under 2 s");
}

/// A file ref keeps each version of a path on the facts index, and reads report whether the
/// file on disk is still the saved one.
#[test]
fn file_refs_keep_versions() {
    let dir = Dir::new();
    let running = support::serve(support::sim_engine(&SimDisk::new(4), |_| {}), &dir, 8);
    let url = format!("http://{}", running.server.tcp_addr().unwrap());
    let token = private_file(&dir, "t", TOKEN_A);
    let args = ["--url", &url, "--token-file", &token, "--agent", "ag", "--session", "s1"];
    let mut mcp = Mcp::start(&dir.0, &args, None);
    mcp.handshake();

    let shots = dir.0.join("shots ü");
    std::fs::create_dir(&shots).unwrap();
    let file = shots.join("shot 1.png");
    std::fs::write(&file, b"first image").unwrap();
    let path = file.to_str().unwrap();
    let key = format!("file{}", std::fs::canonicalize(&file).unwrap().to_str().unwrap());
    let hash = |bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();

    let saved = mcp.call("file_ref", json!({"path": path, "about": "login page, dark mode"}));
    let first = &saved["structuredContent"];
    assert_eq!((&first["key"], &first["unchanged"]), (&json!(key), &json!(false)), "{saved}");
    assert_eq!(first["blake3"], hash(b"first image"));
    let first_lsn = first["lsn"].as_u64().unwrap();
    let again = mcp.call("file_ref", json!({"path": path, "about": "login page, dark mode"}));
    assert_eq!(again["structuredContent"]["unchanged"], true, "same bytes and about: {again}");
    assert_eq!(again["structuredContent"]["lsn"], first_lsn, "nothing written");

    // Touched, not edited: the hash still matches.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(later).unwrap();
    let got = mcp.call("file_get", json!({"path": path}));
    assert_eq!(got["structuredContent"]["ref"]["status"], "ok", "{got}");
    assert_eq!(got["structuredContent"]["ref"]["about"], "login page, dark mode");
    let meta = &got["structuredContent"]["ref"]["file"];
    assert_eq!((&meta["bytes"], &meta["mime"]), (&json!(11), &json!("image/png")));

    std::fs::write(&file, b"second image, wider").unwrap();
    let edited = mcp.call("file_get", json!({"path": path}));
    assert_eq!(edited["structuredContent"]["ref"]["status"], "changed");
    let saved = mcp.call("file_ref", json!({"path": path, "about": "login page, light mode"}));
    let second_lsn = saved["structuredContent"]["lsn"].as_u64().unwrap();
    assert!(second_lsn > first_lsn);
    let versions = mcp.call("file_get", json!({"path": path, "versions": true}));
    let content = &versions["structuredContent"];
    let list = content["versions"].as_array().unwrap();
    let lsns: Vec<u64> = list.iter().map(|v| v["lsn"].as_u64().unwrap()).collect();
    assert_eq!(lsns, [second_lsn, first_lsn], "newest first: {versions}");
    assert_eq!(list[1]["blake3"], hash(b"first image"));
    assert_eq!(list[1]["about"], "login page, dark mode");
    assert_eq!(content["on_disk"], second_lsn);
    assert_eq!(content["ref"]["status"], "ok");

    let found = mcp.call("memory_search", json!({"query": "light"}));
    assert_eq!(text(&found), format!("[fact {key}] login page, light mode"));
    let folder = mcp.call("file_get", json!({"path": shots.to_str().unwrap()}));
    let refs = folder["structuredContent"]["refs"].as_array().unwrap();
    assert_eq!((refs.len(), &refs[0]["key"], &refs[0]["status"]), (1, &json!(key), &json!("ok")));

    std::fs::remove_file(&file).unwrap();
    let gone = mcp.call("file_get", json!({"path": path, "versions": true}));
    let content = &gone["structuredContent"];
    assert_eq!(content["ref"]["status"], "missing", "{gone}");
    assert_eq!(
        (content["versions"].as_array().unwrap().len(), &content["on_disk"]),
        (2, &Value::Null)
    );

    // A deleted file found through a symlink or `..` still reaches the key it was saved under.
    std::os::unix::fs::symlink(&shots, dir.0.join("link")).unwrap();
    let dotted = shots.join("..").join("shots ü").join("shot 1.png");
    for (case, other) in [("symlink", dir.0.join("link/shot 1.png")), ("dot-dot", dotted)] {
        let gone = mcp.call("file_get", json!({"path": other, "versions": true}));
        let content = &gone["structuredContent"];
        assert_eq!(
            (&content["ref"]["key"], &content["ref"]["status"]),
            (&json!(key), &json!("missing")),
            "{case}: {gone}"
        );
        assert_eq!(content["versions"].as_array().unwrap().len(), 2, "{case}");
    }

    // `forget file` finds a deleted file's ref by its path. Then no read shows the ref, and its
    // history holds placeholders only. A new save starts a fresh ref with 1 version.
    let verb = [&["forget", "file", "--path", path][..], &args[..4]].concat();
    let (status, out) = support::Process::spawn(&dir.0, &verb).wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let gone = mcp.call("file_get", json!({"path": path, "versions": true}));
    assert_eq!(gone["structuredContent"], json!({"ref": null}), "{gone}");
    let folder = mcp.call("file_get", json!({"path": shots.to_str().unwrap()}));
    assert_eq!(folder["structuredContent"]["refs"], json!([]), "{folder}");
    assert_eq!(text(&mcp.call("memory_search", json!({"query": "light"}))), "no matches");
    let byte = |b: u8| match b.is_ascii_alphanumeric() || b == b'/' {
        true => char::from(b).to_string(),
        false => format!("%{b:02X}"),
    };
    let target = format!("/v1/facts/history?key={}", key.bytes().map(byte).collect::<String>());
    let history = running.vias[1].a("GET", &target, None).1;
    let hidden = [second_lsn, first_lsn].map(|lsn| json!({"lsn": lsn, "redacted": true}));
    assert_eq!(history["revisions"], json!(hidden), "{history}");
    std::fs::write(&file, b"first image").unwrap();
    let saved = mcp.call("file_ref", json!({"path": path, "about": "login page, dark mode"}));
    assert_eq!(saved["structuredContent"]["unchanged"], false, "a fresh ref: {saved}");
    let fresh = saved["structuredContent"]["lsn"].clone();
    let versions = mcp.call("file_get", json!({"path": path, "versions": true}));
    let content = &versions["structuredContent"];
    let list = content["versions"].as_array().unwrap();
    let lsns: Vec<&Value> = list.iter().map(|version| &version["lsn"]).collect();
    assert_eq!((lsns, &content["on_disk"]), (vec![&fresh], &fresh), "{versions}");

    // The stored about is redacted, so the same save again still writes nothing.
    let notes = dir.0.join("notes.txt");
    std::fs::write(&notes, b"n").unwrap();
    let secret = json!({"path": notes, "about": format!("key sk-{}", "a".repeat(40))});
    let first = mcp.call("file_ref", secret.clone());
    assert_eq!(first["structuredContent"]["unchanged"], false, "{first}");
    let again = mcp.call("file_ref", secret);
    assert_eq!(again["structuredContent"]["unchanged"], true, "redacted about: {again}");

    // A 1021-byte path: its key, `file` and the path, is 1025 bytes. macOS caps paths at 1024.
    let mut deep = std::fs::canonicalize(&shots).unwrap();
    loop {
        let left = 1021 - deep.as_os_str().len();
        if left <= 201 {
            deep = deep.join("z".repeat(left - 1));
            break;
        }
        deep = deep.join("y".repeat(199));
        std::fs::create_dir(&deep).unwrap();
    }
    std::fs::write(&deep, b"z").unwrap();
    let missing = dir.0.join("nope.png");
    for (case, args, expect) in [
        ("missing file", json!({"path": missing, "about": "x"}), "path not found"),
        ("folder", json!({"path": shots, "about": "x"}), "path must be a file"),
        ("empty about", json!({"path": deep, "about": " "}), "about is required"),
        ("no path", json!({"about": "x"}), "path is required"),
        (
            "long key",
            json!({"path": deep, "about": "x"}),
            "path is too long: its key is over 1024 bytes",
        ),
    ] {
        let rejected = mcp.call("file_ref", args);
        assert_eq!(rejected["isError"], true, "{case}: {rejected}");
        assert!(text(&rejected).starts_with(expect), "{case}: {rejected}");
    }
    let page = mcp.call("fact_list", json!({"prefix": "file/"}));
    assert_eq!(page["structuredContent"]["facts"].as_array().unwrap().len(), 2, "nothing else");
    assert_eq!(mcp.close().0.code(), Some(0));
}

/// A server, its store flags, and 2 adapters on it as agents `a` and `b`.
fn two_writers(dir: &Dir) -> (support::Running, Vec<String>, Mcp, Mcp) {
    let running = support::serve(support::sim_engine(&SimDisk::new(4), |_| {}), dir, 8);
    let url = format!("http://{}", running.server.tcp_addr().unwrap());
    let flags = vec!["--url".into(), url, "--token-file".into(), private_file(dir, "t", TOKEN_A)];
    let start = |agent: &str| {
        let args: Vec<&str> = flags.iter().map(String::as_str).collect();
        let mut mcp = Mcp::start(&dir.0, &[&args[..], &["--agent", agent]].concat(), None);
        mcp.handshake();
        mcp
    };
    let (a, b) = (start("a"), start("b"));
    (running, flags, a, b)
}

fn committed(result: &Value, case: &str) {
    assert_eq!(result["structuredContent"]["committed"], true, "{case}: {result}");
}

/// Checks a `fact_set` refused because the key changed: the reason, then the current value.
/// Checks a conflict error. A `by` line's time reads as `<t>` in `current`. Returns that time.
fn conflicted(result: &Value, current: &[&str], case: &str) -> Option<String> {
    assert_eq!(result["isError"], true, "{case}: {result}");
    let first = "conflict: this fact changed, or you haven't read it in this chat yet. Nothing \
                 was written. Its current value is below. Merge it with your change and call \
                 fact_set again.";
    let mut time = None;
    let lines: Vec<String> = text(result)
        .lines()
        .map(|line| match line.starts_with("by ") {
            true => {
                let (line, at) = timed(line);
                time = Some(at);
                line
            }
            false => line.to_owned(),
        })
        .collect();
    assert_eq!(lines, [&[first][..], current].concat(), "{case}");
    time
}

/// `line` with the UTC time after its first ` at ` put as `<t>`, and that time.
fn timed(line: &str) -> (String, String) {
    let (head, tail) = line.split_once(" at ").unwrap_or_else(|| panic!("no time: {line}"));
    let time = tail.get(..20).unwrap_or_else(|| panic!("short time: {line}"));
    let shape = time.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        10 => b == b'T',
        13 | 16 => b == b':',
        19 => b == b'Z',
        _ => b.is_ascii_digit(),
    });
    assert!(shape, "not a UTC time: {line}");
    (format!("{head} at <t>{}", &tail[20..]), time.to_owned())
}

/// An adapter writes a key only at the LSN it last saw, 0 for a key it never read.
#[test]
fn fact_set_never_overwrites_a_value_it_has_not_seen() {
    let dir = Dir::new();
    let (_running, _, mut a, mut b) = two_writers(&dir);
    let set = |mcp: &mut Mcp, content: &str| {
        mcp.call("fact_set", json!({"key": "repo/k", "content": content}))
    };
    committed(&set(&mut a, "one"), "a sets an absent key");
    conflicted(&set(&mut b, "two"), &["one", "by a at <t>"], "b never read it");
    committed(&set(&mut b, "two"), "b saw a's value in the conflict");
    conflicted(&set(&mut a, "three"), &["two", "by b at <t>"], "b changed it after a's write");
    assert_eq!(text(&a.call("fact_get", json!({"key": "repo/k"}))), "two");
    committed(&set(&mut a, "three"), "a read b's value");
    let delete = a.call("fact_set", json!({"key": "repo/k", "delete": true}));
    committed(&delete, "a deletes its own value");
    committed(&set(&mut a, "four"), "a saw the key go");
    assert_eq!(text(&b.call("fact_get", json!({"key": "repo/k"}))), "four");
    // b last saw four. A delete by a leaves no current value.
    committed(&a.call("fact_set", json!({"key": "repo/k", "delete": true})), "a deletes four");
    conflicted(&set(&mut b, "five"), &["no current value"], "a deleted it");
    committed(&set(&mut b, "five"), "b saw the delete");
    // A listed key counts as read.
    a.call("fact_list", json!({"prefix": "repo/"}));
    committed(&set(&mut a, "six"), "a listed it");
    for mcp in [&mut a, &mut b] {
        let (status, _) = mcp.close();
        assert_eq!(status.code(), Some(0), "{}", mcp.process.stderr());
        assert!(!mcp.process.stdout().contains("if_lsn"), "the model never sees if_lsn");
    }
}

/// The shell verbs keep no LSNs, so they overwrite whatever is there.
#[test]
fn shell_fact_set_stays_last_write_wins() {
    let dir = Dir::new();
    let (_running, flags, mut a, _) = two_writers(&dir);
    committed(&a.call("fact_set", json!({"key": "repo/k", "content": "one"})), "mcp a");
    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let shell = |verb: &[&str]| {
        let mut process = Process::spawn(&dir.0, &[verb, &flags[..]].concat());
        let (status, out) = process.wait();
        assert_eq!(status.code(), Some(0), "{verb:?}: {out}");
        serde_json::from_str::<Value>(&process.stdout()).unwrap()
    };
    for content in ["two", "three"] {
        let set = shell(&["fact", "set", "--key", "repo/k", "--content", content]);
        assert_eq!(set["committed"], true, "shell sets {content}");
    }
    let got = shell(&["fact", "get", "--key", "repo/k"]);
    assert_eq!(got["fact"]["content"], "three");
}

/// `file_ref` reads its key before it writes, so another writer's ref never blocks it.
#[test]
fn file_ref_after_another_writer_still_saves() {
    let dir = Dir::new();
    let (_running, _, mut a, mut b) = two_writers(&dir);
    let file = dir.0.join("shot.png");
    std::fs::write(&file, b"image").unwrap();
    let key = format!("file{}", std::fs::canonicalize(&file).unwrap().to_str().unwrap());
    committed(&b.call("fact_set", json!({"key": key, "content": "by b"})), "b sets the ref");
    let saved = a.call("file_ref", json!({"path": file, "about": "login page"}));
    committed(&saved, "a saves over b's ref");
    assert_eq!(saved["structuredContent"]["unchanged"], false, "{saved}");
}

/// `fact_history` lists a key's revisions newest first, with who wrote each. It is no read
/// of the current value, so `fact_set` still refuses a key known only from its history.
#[test]
fn fact_history_shows_who_changed_a_fact() {
    let dir = Dir::new();
    let (_running, flags, mut a, mut b) = two_writers(&dir);
    let one = fact_written(&a.call("fact_set", json!({"key": "repo/h", "content": "one"})), "1");
    let two = fact_written(&a.call("fact_set", json!({"key": "repo/h", "content": "two"})), "2");
    b.call("fact_history", json!({"key": "repo/h"}));
    let set_two = conflicted(
        &b.call("fact_set", json!({"key": "repo/h", "content": "b"})),
        &["two", "by a at <t>"],
        "history is no read",
    );
    let gone = fact_written(&a.call("fact_set", json!({"key": "repo/h", "delete": true})), "del");
    let history = b.call("fact_history", json!({"key": "repo/h"}));
    let (lines, times): (Vec<String>, Vec<String>) = text(&history).lines().map(timed).unzip();
    assert_eq!(
        lines,
        [
            format!("lsn {gone} at <t> by a: (deleted)"),
            format!("lsn {two} at <t> by a: two"),
            format!("lsn {one} at <t> by a: one"),
        ]
    );
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]), "newest first: {times:?}");
    assert_eq!(set_two.as_ref(), Some(&times[1]), "the conflict and history agree on the time");
    let structured = &history["structuredContent"];
    let revisions = structured["revisions"].as_array().unwrap();
    let field = |name: &str| revisions.iter().map(|r| r[name].clone()).collect::<Vec<_>>();
    assert_eq!(field("lsn"), [json!(gone), json!(two), json!(one)], "newest first");
    assert_eq!(field("deleted"), [json!(true), json!(false), json!(false)]);
    assert_eq!(field("content"), [json!(""), json!("two"), json!("one")]);
    assert_eq!(field("agent"), [json!("a"), json!("a"), json!("a")]);
    assert_eq!(structured["truncated"], false);

    let none = b.call("fact_history", json!({"key": "repo/none"}));
    assert_eq!(none["isError"], Value::Null, "{none}");
    assert_eq!(text(&none), "no revisions");
    let nothing = json!({"revisions": [], "truncated": false, "next": null});
    assert_eq!(none["structuredContent"], nothing);

    let cut = b.call("fact_history", json!({"key": "repo/h", "max_bytes": 1}));
    assert_eq!(cut["structuredContent"]["truncated"], true, "{cut}");
    assert_eq!(cut["structuredContent"]["next"], gone, "{cut}");
    let lines: Vec<&str> = text(&cut).lines().collect();
    let hint = format!("more: call fact_history again with before={gone}");
    assert_eq!(lines.last(), Some(&hint.as_str()));

    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let lsns = one.to_string();
    let mut forget =
        Process::spawn(&dir.0, &[&["forget", "lsns", "--lsns", &lsns], &flags[..]].concat());
    let (status, out) = forget.wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let history = b.call("fact_history", json!({"key": "repo/h"}));
    let lines: Vec<&str> = text(&history).lines().collect();
    assert_eq!(lines.last(), Some(&format!("lsn {one} (forgotten)").as_str()), "{history}");
    let oldest = &history["structuredContent"]["revisions"][2];
    assert_eq!(oldest, &json!({"lsn": one, "redacted": true}));

    let mut shell =
        Process::spawn(&dir.0, &[&["fact", "history", "--key", "repo/h"], &flags[..]].concat());
    let (status, out) = shell.wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let printed: Value = serde_json::from_str(&shell.stdout()).unwrap();
    assert_eq!(printed["revisions"][0]["lsn"], gone, "the shell verb prints the same JSON");
    for mcp in [&mut a, &mut b] {
        assert_eq!(mcp.close().0.code(), Some(0), "{}", mcp.process.stderr());
    }
}

/// `fact_history` past the 16 revisions the index keeps: the text names `before`, and the
/// shell verb takes `--before`.
#[test]
fn fact_history_pages_past_the_index_cap() {
    let dir = Dir::new();
    let (_running, flags, mut a, _) = two_writers(&dir);
    for n in 1..=20 {
        let set = a.call("fact_set", json!({"key": "repo/p", "content": n.to_string()}));
        fact_written(&set, &format!("set {n}"));
    }
    let first = a.call("fact_history", json!({"key": "repo/p"}));
    let next = first["structuredContent"]["next"].as_u64().unwrap_or_else(|| panic!("{first}"));
    let lines: Vec<&str> = text(&first).lines().collect();
    let hint = format!("more: call fact_history again with before={next}");
    assert_eq!(lines.last(), Some(&hint.as_str()), "{first}");
    assert_eq!(first["structuredContent"]["revisions"].as_array().unwrap().len(), 16);
    let older = a.call("fact_history", json!({"key": "repo/p", "before": next}));
    let revisions = older["structuredContent"]["revisions"].as_array().unwrap();
    let contents: Vec<&Value> = revisions.iter().map(|r| &r["content"]).collect();
    assert_eq!(contents, [&json!("4"), &json!("3"), &json!("2"), &json!("1")], "{older}");
    assert!(!text(&older).contains("more:"), "the last page: {older}");
    assert_eq!(older["structuredContent"]["next"], Value::Null);

    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
    let verb =
        ["fact", "history", "--key", "repo/p", "--before", &next.to_string()].map(str::to_owned);
    let verb: Vec<&str> = verb.iter().map(String::as_str).collect();
    let mut shell = Process::spawn(&dir.0, &[&verb[..], &flags[..]].concat());
    let (status, out) = shell.wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let printed: Value = serde_json::from_str(&shell.stdout()).unwrap();
    assert_eq!(printed["revisions"].as_array().unwrap().len(), 4, "{printed}");
    assert_eq!(a.close().0.code(), Some(0), "{}", a.process.stderr());
}

/// Adapters find or start one server, and send the token only to a server they trust.
#[test]
fn launch_race_starts_one_server() {
    let dir = Dir::new();
    let token = init_config(&dir);
    let data = dir.0.join("d");
    let _holders = Holders(vec![data.clone(), dir.0.join("B"), dir.0.join("U")]);
    let args = ["--config", "c.json", "--tenant", "a", "--data-dir", "d"];

    // Two adapters race to start a server on an empty data directory.
    let mut racers = [0, 1].map(|_| Mcp::start(&dir.0, &args, None));
    for mcp in &mut racers {
        mcp.handshake();
    }
    for mcp in &mut racers {
        mcp.post_call("memory_append", json!({"content": "race"}));
    }
    for mcp in &mut racers {
        appended(&mcp.response()["result"], "racer");
    }
    // JSON escapes each NUL to 6 bytes, so a 2 MiB window is a 12 MiB response.
    let nul = "\0".repeat(1 << 20);
    for _ in 0..2 {
        appended(&racers[0].call("memory_append", json!({"content": nul})), "NUL");
    }
    let window = racers[0].call("memory_recent", json!({"n": 2, "max_bytes": 2 << 20}));
    assert_eq!(contents(&window), [nul.as_str(); 2], "{}", text(&window).len());
    // The loser says the directory is locked and exits 75. It may still be starting when both
    // adapters already trust the winner, so wait for its line.
    let log = || std::fs::read_to_string(data.join("serve.log")).unwrap();
    wait_for("the losing server exits", || log().contains(" is locked by "));
    assert_eq!(log().matches("atticdb: serving").count(), 1, "one server: {}", log());
    assert!(!log().contains("in use"), "never EADDRINUSE: {}", log());
    assert!(!log().contains(" POST /v1/append "), "requests are not logged: {}", log());
    let stderr: String = racers.iter().map(|mcp| mcp.process.stderr()).collect();
    let exits = stderr.lines().filter(|line| line.contains("exited:"));
    assert!(exits.into_iter().all(|line| line.ends_with("exit status: 75")), "{stderr}");
    for mcp in &mut racers {
        assert_eq!(mcp.close().0.code(), Some(0));
    }

    // Each row stops the server, then points `LOCK` at something a fresh adapter must not
    // trust. The adapter refuses it without sending the token, starts its own server, and
    // commits. `true` marks a fake listener the adapter probes, `false` one it never touches.
    // An empty refusal marks a server that is just not running, which stderr must not report.
    type Setup = fn(&Path, &Path, &str) -> (Option<String>, Vec<(Seen, bool)>);
    let rows: &[(&str, Setup, &str)] = &[
        (
            "a listener with another instance id, and one on the TCP address with the right id",
            |root, _, instance| {
                let socket = root.join("f.sock");
                let other = fake_unix(&socket, &"f".repeat(32));
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let tcp = listener.local_addr().unwrap().to_string();
                let stale = fake(move || listener.accept().map(|(stream, _)| stream), instance);
                (Some(lock_text(instance, &socket, &tcp)), vec![(other, true), (stale, false)])
            },
            "is not instance",
        ),
        (
            "a group-writable socket directory",
            |root, _, instance| {
                let parent = root.join("g");
                std::fs::create_dir(&parent).unwrap();
                chmod(&parent, 0o770);
                let seen = fake_unix(&parent.join("s"), instance);
                (Some(lock_text(instance, &parent.join("s"), "none")), vec![(seen, false)])
            },
            "not writable by group or others",
        ),
        (
            "a group-writable grandparent",
            |root, _, instance| {
                let parent = root.join("gw").join("p");
                std::fs::create_dir_all(&parent).unwrap();
                chmod(&root.join("gw"), 0o770);
                let seen = fake_unix(&parent.join("s"), instance);
                (Some(lock_text(instance, &parent.join("s"), "none")), vec![(seen, false)])
            },
            "not writable by group or others unless sticky",
        ),
        (
            FOREIGN_ROW,
            |_, _, instance| (Some(lock_text(instance, foreign_socket().unwrap(), "none")), vec![]),
            "must be a socket owned by uid",
        ),
        (
            "a killed server's stale socket and leftover staging directory",
            |_, data, instance| {
                // A start killed while installing leaves `.zs.<first 8 hex of its instance>`.
                let staging = data.join(format!(".zs.{}", &instance[..8]));
                std::fs::create_dir(&staging).unwrap();
                std::fs::write(staging.join("s"), "").unwrap();
                (None, vec![])
            },
            "",
        ),
    ];
    for (case, setup, refusal) in rows {
        if *case == FOREIGN_ROW && foreign_socket().is_none() {
            let why = format!("none of {FOREIGN_SOCKETS:?} is a socket owned by another user");
            // GitHub runners have these sockets and run as non-root, so CI always covers it.
            assert!(std::env::var_os("CI").is_none(), "{case}: {why}");
            eprintln!("SKIPPED ROW {case:?}: {why}. Run as non-root on a stock host.");
            continue;
        }
        let old = lock_field(&data, "instance=");
        assert!(kill_holder(&data), "{case}: stop the server");
        let (lock, fakes) = setup(&dir.0, &data, &old);
        if let Some(lock) = lock {
            std::fs::write(data.join("LOCK"), lock).unwrap();
        }
        let mut mcp = Mcp::start(&dir.0, &args, None);
        mcp.handshake();
        appended(&mcp.call("memory_append", json!({"content": case})), case);
        let stderr = mcp.process.stderr();
        if refusal.is_empty() {
            assert!(!stderr.contains("no trusted server"), "{case}: {stderr}");
        } else {
            assert!(stderr.contains(refusal), "{case}: {stderr}");
        }
        assert_ne!(lock_field(&data, "instance="), old, "{case}: the adapter started a server");
        for (seen, probed) in fakes {
            let connections = seen.connections.load(Ordering::SeqCst);
            assert_eq!(connections > 0, probed, "{case}: {connections} connections");
            assert_eq!(seen.authorized.load(Ordering::SeqCst), 0, "{case}: token sent");
        }
        assert_eq!(mcp.close().0.code(), Some(0), "{case}");
    }

    // A poisoned `--exit-on-poison` holder that is slow to exit: it stopped accepting, but a
    // response it is still writing keeps it alive. Spawned servers lose the lock race until it
    // exits, and the adapter keeps spawning until one wins.
    #[cfg(feature = "fault-inject")]
    {
        use crate::support::Via;
        use std::os::unix::net::UnixStream;
        assert!(kill_holder(&data));
        let serve = ["serve", "--config", "c.json", "--data-dir", "d", "--listen", "none"];
        let mut holder =
            support::Process::spawn(&dir.0, &[&serve[..], &["--exit-on-poison"]].concat());
        holder.ready();
        let socket = data.join(atticdb::listen::SOCKET_NAME);
        let via = Via::Unix(socket.clone());
        let mut big = support::append_body("big", "big", &"x".repeat(2 << 20));
        big["agent"] = json!("x");
        assert_eq!(via.call(None, Some(&token), "POST", "/v1/append", Some(&big)).0, 200);
        let mut slow = UnixStream::connect(&socket).unwrap();
        let target = "/v1/window?agent=x&session=big&last=1&max_bytes=8388608";
        write!(slow, "GET {target} HTTP/1.1\r\nAuthorization: Bearer {token}\r\n\r\n").unwrap();
        slow.read_exact(&mut [0]).unwrap();
        std::fs::write(data.join(atticdb::fault::TRIGGER), "").unwrap();
        let poison = support::append_body("s", "poison", "p");
        let unknown = json!({"outcome": "unknown", "error": "storage"});
        assert_eq!(
            via.call(None, Some(&token), "POST", "/v1/append", Some(&poison)),
            (500, unknown)
        );
        let mut mcp = Mcp::start(&dir.0, &args, None);
        mcp.handshake();
        mcp.post_call("memory_append", json!({"content": "after poison"}));
        let lost = || mcp.process.stderr().matches("exited: exit status: 75").count();
        wait_for("spawned servers lose to the slow holder twice", || lost() >= 2);
        assert!(holder.child.try_wait().unwrap().is_none(), "the holder is still exiting");
        drop(slow);
        let (status, output) = holder.wait();
        assert_eq!(status.code(), Some(70), "{output}");
        appended(&mcp.response()["result"], "after the slow holder");
        assert_eq!(mcp.close().0.code(), Some(0));
    }

    // A config naming directory A, overridden by `--data-dir`. A relative `unix_socket` is
    // in the adapter's folder, though the started server runs in `/`.
    let tenants = json!([{"name": "a", "token": token}]);
    let rows = [
        ("--data-dir B", "B", json!({"data_dir": dir.0.join("A"), "tenants": tenants}), None),
        (
            "relative unix_socket",
            "U",
            json!({"unix_socket": "u.sock", "tenants": tenants}),
            Some("u.sock"),
        ),
    ];
    for (case, data, config, socket) in rows {
        let name = format!("{data}.json");
        private_file(&dir, &name, &config.to_string());
        let args = ["--config", &name, "--tenant", "a", "--data-dir", data];
        let mut mcp = Mcp::start(&dir.0, &args, None);
        mcp.handshake();
        appended(&mcp.call("memory_append", json!({"content": "b"})), case);
        let socket =
            socket.map_or(dir.0.join(data).join(atticdb::listen::SOCKET_NAME), |s| dir.0.join(s));
        assert_eq!(lock_field(&dir.0.join(data), "unix="), socket.display().to_string(), "{case}");
        assert_eq!(mcp.close().0.code(), Some(0), "{case}");
    }
    assert!(!dir.0.join("A").exists(), "the config's directory is untouched");
}

/// With no connection flags, the shell verbs create and use a per-user home, ignoring a stray
/// `ATTICDB_TOKEN` with a note. 4 parallel first runs all succeed and leave no temp config.
/// One left by a killed run is ignored. `ATTICDB_HOME` moves the home, and `HOME` stays
/// empty. It needs no `HOME`. An empty `ATTICDB_HOME` counts as unset.
#[test]
fn no_flags_use_the_home() {
    let dir = Dir::new();
    let (stray, stray_row) = (".atticdb.json.0123456789abcdef", "stray temp left alone");
    let mut rows = vec![];
    if cfg!(target_os = "linux") {
        let xdg = Some(("XDG_DATA_HOME", dir.0.join("x")));
        rows.push(("XDG_DATA_HOME", Some(dir.0.clone()), xdg, dir.0.join("x/atticdb")));
    }
    let home =
        if cfg!(target_os = "macos") { "Library/Application Support" } else { ".local/share" };
    rows.push(("HOME", Some(dir.0.clone()), None, dir.0.join(home).join("atticdb")));
    let t = dir.0.join("t");
    rows.push((stray_row, Some(t.clone()), None, t.join(home).join("atticdb")));
    let moved = |name| Some(("ATTICDB_HOME", dir.0.join(name)));
    rows.push(("ATTICDB_HOME", Some(dir.0.join("f")), moved("z"), dir.0.join("z")));
    rows.push(("ATTICDB_HOME, HOME unset", None, moved("y"), dir.0.join("y")));
    let _holders = Holders(rows.iter().map(|(.., home)| home.join("data")).collect());
    for (case, home_env, var, home) in rows {
        let staging = home.join("data/.zs.01234567/s");
        assert!(
            staging.as_os_str().len() < atticdb::listen::SUN_PATH_BYTES,
            "{case}: {} is too deep, set ATTICDB_TEST_DIR to a short directory",
            staging.display()
        );
        home_env.iter().for_each(|home_env| std::fs::create_dir_all(home_env).unwrap());
        if case == stray_row {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&home).unwrap();
            std::fs::write(home.join(stray), "").unwrap();
        }
        let start = |verb: &[&str]| {
            let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
            command.args(verb).current_dir(&dir.0).env("ATTICDB_TOKEN", "x");
            command.envs(home_env.iter().map(|home_env| ("HOME", home_env)));
            command.env("ATTICDB_HOME", "").envs(var.iter().map(|(name, value)| (name, value)));
            Process::start(command.stdin(std::process::Stdio::null()))
        };
        let note = "atticdb: ignoring ATTICDB_TOKEN: no --url, --data-dir or --config given, so \
            the home's token is used\n";
        let run = |mut process: Process| {
            let (status, out) = process.wait();
            assert!(status.success() && process.stderr().starts_with(note), "{case}: {out}");
            serde_json::from_str::<Value>(&process.stdout()).unwrap()
        };
        let written: Vec<String> = (0..4).map(|n| format!("{case} {n}")).collect();
        let started: Vec<_> = written.iter().map(|n| start(&["append", "--content", n])).collect();
        started.into_iter().for_each(|process| _ = run(process));
        let config = std::fs::read(home.join("atticdb.json")).unwrap();
        let recent = run(start(&["recent", "--n", "10"]));
        let turns = recent["turns"].as_array().unwrap();
        let mut found: Vec<_> =
            turns.iter().map(|turn| turn["content"].as_str().unwrap()).collect();
        found.sort_unstable();
        assert_eq!(found, written, "{case}: all 4 in its own home");
        assert_eq!(std::fs::read(home.join("atticdb.json")).unwrap(), config, "{case}: kept");
        let mut names: Vec<_> =
            std::fs::read_dir(&home).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        names.sort_unstable();
        let expected = [stray, "atticdb.json", "data"];
        assert_eq!(names, expected[usize::from(case != stray_row)..], "{case}: no temp left");
        for (path, mode) in [("", 0o700), ("data", 0o700), ("atticdb.json", 0o600)] {
            let found = std::fs::metadata(home.join(path)).unwrap().mode() & 0o777;
            assert_eq!(found, mode, "{case}: {path:?} mode");
        }
        if let Some(home_env) = home_env.filter(|home_env| !home.starts_with(home_env)) {
            let left = std::fs::read_dir(&home_env).unwrap().count();
            assert_eq!(left, 0, "{case}: nothing in HOME");
        }
    }
    // No home is made where it can't be used. Any connection flag keeps the old rule, so no
    // token is an error. A relative ATTICDB_HOME is a usage error, with no fallback to HOME.
    // A home made in a parent that can't be read fails, as its new entry can't be synced.
    let mut rows: Vec<(&[&str], String, i32, String)> = vec![
        (&["--url", "unix:///nowhere.sock"], "".into(), 1, "set ATTICDB_TOKEN".into()),
        (&["--data-dir", "d"], "".into(), 1, "set ATTICDB_TOKEN".into()),
        (&[], "r".into(), 2, "atticdb: ATTICDB_HOME must be an absolute path\n".into()),
    ];
    let unreadable = dir.0.join("p");
    // Root reads any directory, so the row needs a normal user.
    if !rustix::process::geteuid().is_root() {
        std::fs::create_dir(&unreadable).unwrap();
        chmod(&unreadable, 0o300);
        let says = format!(
            "atticdb: {}: cannot sync the entry for the directory made in it",
            unreadable.display()
        );
        rows.push((&[], unreadable.join("new").display().to_string(), 1, says));
    }
    for (flags, atticdb_home, code, says) in rows {
        let (case, home_env) =
            (format!("{flags:?} ATTICDB_HOME={atticdb_home:?}"), dir.0.join("u"));
        let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
        command.arg("recent").args(flags).current_dir(&dir.0).env("HOME", &home_env);
        command.env_remove("ATTICDB_TOKEN").env("ATTICDB_HOME", &atticdb_home);
        let (status, out) = Process::start(command.stdin(std::process::Stdio::null())).wait();
        assert_eq!(status.code(), Some(code), "{case}: {out}");
        assert!(out.contains(&says), "{case}: {out}");
        assert!(!home_env.exists(), "{case}: no home");
    }
    // Dir can then remove it.
    if unreadable.exists() {
        chmod(&unreadable, 0o700);
    }
}

/// A write whose first answer is `unknown` is never reported as `not_committed`.
#[cfg(feature = "fault-inject")]
#[test]
fn unknown_outcome_never_downgrades() {
    // An auto-started server answers unknown and exits 70. The adapter starts another and
    // retries the same key. Only that key explains one copy and the dedup flag.
    let dir = Dir::new();
    init_config(&dir);
    let data = dir.0.join("d");
    let _holders = Holders(vec![data.clone()]);
    let mut mcp =
        Mcp::start(&dir.0, &["--config", "c.json", "--tenant", "a", "--data-dir", "d"], None);
    mcp.handshake();
    appended(&mcp.call("memory_append", json!({"content": "warm"})), "warm");
    std::fs::write(data.join(atticdb::fault::TRIGGER), "").unwrap();
    let result = mcp.call("memory_append", json!({"content": "x"}));
    appended(&result, "after the poison exit");
    assert_eq!(result["structuredContent"]["deduplicated"], true, "{result}");
    let stderr = mcp.process.stderr();
    assert!(stderr.contains("exited: exit status: 70"), "the server exited on poison: {stderr}");
    assert_eq!(contents(&mcp.call("memory_recent", json!({}))), ["warm", "x"], "one copy");
    std::fs::write(data.join(atticdb::fault::TRIGGER), "").unwrap();
    let result = mcp.call("fact_set", json!({"key": "rule", "content": "x"}));
    fact_written(&result, "fact_set after the poison exit");
    assert_eq!(result["structuredContent"]["deduplicated"], true, "{result}");
    let exits = mcp.process.stderr().matches("exited: exit status: 70").count();
    assert_eq!(exits, 2, "the server exited on poison again");
    assert_eq!(text(&mcp.call("fact_get", json!({"key": "rule"}))), "x");
    assert_eq!(mcp.close().0.code(), Some(0));

    // A server without `--exit-on-poison` and an adapter with `--url`: the first answer is
    // unknown, retries get not_committed from the poisoned server.
    let dir = Dir::new();
    let token = init_config(&dir);
    let data = dir.0.join("d");
    // The test reads request lines to see the unknown answer and the rejected retries.
    let serve =
        ["serve", "--config", "c.json", "--data-dir", "d", "--listen", "none", "--log-requests"];
    let url = format!("unix://{}", data.join(atticdb::listen::SOCKET_NAME).display());
    for (case, restart) in [("restarted after 1 s", true), ("no restart, 3 s deadline", false)] {
        let mut server = support::Process::spawn(&dir.0, &serve);
        server.ready();
        let deadline = if restart { "30s" } else { "3s" };
        let args = ["--url", &url, "--retry-deadline", deadline];
        let mut mcp = Mcp::start(&dir.0, &args, Some(&token));
        mcp.handshake();
        std::fs::write(data.join(atticdb::fault::TRIGGER), "").unwrap();
        mcp.post_call("memory_append", json!({"content": case}));
        let (unknown, rejected) = ("POST /v1/append 500", "POST /v1/append 503");
        wait_for("a retry gets not_committed", || server.stderr().contains(rejected));
        assert!(server.stderr().contains(unknown), "{case}: first answer unknown");
        let result = if restart {
            std::thread::sleep(std::time::Duration::from_secs(1));
            server.child.kill().unwrap();
            server.wait();
            server = support::Process::spawn(&dir.0, &serve);
            server.ready();
            let result = mcp.response()["result"].clone();
            appended(&result, case);
            assert_eq!(result["structuredContent"]["deduplicated"], true, "{case}: {result}");
            let window = mcp.call("memory_recent", json!({}));
            assert_eq!(contents(&window).iter().filter(|c| **c == case).count(), 1, "{case}");
            result
        } else {
            mcp.response()["result"].clone()
        };
        if !restart {
            assert_eq!(result["isError"], true, "{case}: {result}");
            let expect = "unknown: the write may be saved, and repeating the call may duplicate it";
            assert!(text(&result).starts_with(expect), "{case}: {result}");
            assert!(!text(&result).contains("nothing was written"), "{case}");
        }
        assert_eq!(mcp.close().0.code(), Some(0), "{case}");
    }
}

/// The guide goes into every chat-start guide, so its size is budgeted. It had 240 words before
/// it asked for reasons with their evidence. It may grow by at most 30.
#[test]
fn instructions_stay_within_word_budget() {
    let words = atticdb::mcp::INSTRUCTIONS.split_whitespace().count();
    assert!(words <= 240 + 30, "INSTRUCTIONS has {words} words, over the budget of 270");
}
