use crate::support::{
    chmod, cwd, data_dir, files, hook, private_file, seed, slow_store, wait_for, Dir, Holders,
    Process, Via, TOKEN_A, TOKEN_B,
};
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CSP: &str = concat!(
    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; ",
    "connect-src 'self'; worker-src 'none'; base-uri 'none'; form-action 'none'; ",
    "frame-ancestors 'none'"
);

/// A turn written after the page loaded, so a forget pinned to the page leaves it.
const LATE: &str = "written after the page loaded";

/// On every reply, errors included.
const HEADERS: [(&str, &str); 6] = [
    ("Connection", "close"),
    ("Content-Security-Policy", CSP),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("Cache-Control", "no-store"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
];

/// A reply off the wire.
struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    /// Parses 1 reply and checks the headers every reply carries.
    fn parse(case: &str, wire: &str) -> Self {
        let (head, body) =
            wire.split_once("\r\n\r\n").unwrap_or_else(|| panic!("{case}: {wire:?}"));
        let status = head.get(9..12).and_then(|code| code.parse().ok());
        let status = status.unwrap_or_else(|| panic!("{case}: {head}"));
        let reply = Self { status, head: head.into(), body: body.into() };
        for (name, value) in HEADERS {
            assert_eq!(reply.header(name), Some(value), "{case}: {name}");
        }
        assert_eq!(wire.matches("HTTP/1.1 ").count(), 1, "{case}: 1 reply per connection");
        reply
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.body))
    }
}

/// A running `atticdb studio`, killed on drop.
struct Studio {
    process: Process,
    port: u16,
    secret: String,
    /// Every reply, as it came off the wire.
    wires: Mutex<Vec<String>>,
}

impl Studio {
    /// Runs `atticdb studio` and waits for its link.
    fn start(home: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Self {
        let mut process = Process::start(&mut command(home, args, env));
        let deadline = Instant::now() + Duration::from_secs(60);
        let link = loop {
            let out = process.stdout();
            if let Some(link) = out.lines().find_map(|line| line.strip_prefix("atticdb studio: ")) {
                break link.to_owned();
            }
            let exited = process.child.try_wait().unwrap();
            assert!(exited.is_none() && Instant::now() < deadline, "no link: {}", process.stderr());
            std::thread::sleep(Duration::from_millis(10));
        };
        let rest = link.strip_prefix("http://127.0.0.1:").unwrap();
        let (port, secret) = rest.split_once("/#k=").unwrap();
        let hex = secret.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        assert!(secret.len() == 32 && hex, "32 lowercase hex: {link}");
        let (port, secret) = (port.parse().unwrap(), secret.to_owned());
        Self { process, port, secret, wires: Mutex::default() }
    }

    /// Sends `request` as written.
    fn send(&self, case: &str, request: &str) -> Reply {
        let wire = Via::Tcp(SocketAddr::from(([127, 0, 0, 1], self.port))).raw(request.as_bytes());
        self.wires.lock().unwrap().push(wire.clone());
        Reply::parse(case, &wire)
    }

    /// `GET target` with the right `Host`, and with the secret when `secret`.
    fn get(&self, target: &str, secret: bool) -> Reply {
        let key = if secret { format!("X-Atticdb-Studio: {}\r\n", self.secret) } else { "".into() };
        let host = format!("Host: 127.0.0.1:{}\r\n", self.port);
        self.send(target, &format!("GET {target} HTTP/1.1\r\n{host}{key}\r\n"))
    }

    /// `POST /api/forget` with `body`, and the headers the page sends.
    fn forget(&self, case: &str, body: &Value) -> Reply {
        let (port, secret, body) = (self.port, &self.secret, body.to_string());
        let head = format!(
            "POST /api/forget HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Atticdb-Studio: {secret}\r\n\
             Origin: http://127.0.0.1:{port}\r\nSec-Fetch-Site: same-origin\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        self.send(case, &(head + &body))
    }
}

/// `atticdb studio` with `HOME` set to `home`, and `env` added.
fn command(home: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Command {
    let mut command = crate::support::command(env!("CARGO_BIN_EXE_atticdb"));
    command.arg("studio").args(args).current_dir(home).env("HOME", home).stdin(Stdio::null());
    command.env_remove("ATTICDB_TOKEN").envs(env.iter().copied());
    command
}

/// The paths `html` links with `src` or `href` on its own server, relative to `/`. Not a
/// fragment, a `data:` URL or another origin.
fn links(html: &str) -> Vec<String> {
    let mut links = Vec::new();
    for (attr, quote) in [("src=\"", '"'), ("src='", '\''), ("href=\"", '"'), ("href='", '\'')] {
        for (at, _) in html.match_indices(attr) {
            let value = html[at + attr.len()..].split(quote).next().unwrap_or_default();
            if !value.starts_with('#') && !value.starts_with("//") && !value.contains(':') {
                links.push(value.trim_start_matches('/').to_owned());
            }
        }
    }
    links
}

/// Studios on the home, on its data directory and on a `--url`. Each serves its page, forwards
/// reads with the token it holds, and refuses everything else, with the same headers. The page
/// files show store text only as text.
#[test]
fn studio_forwards_reads_and_refuses_the_rest() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir_all(home.join("app/.git")).unwrap();
    let data = data_dir(&home);
    let _holders = Holders(vec![data.clone()]);
    seed(&home, &["fact", "set", "--key", "user/style", "--content", "short sentences"]);
    seed(&home, &["fact", "set", "--key", "repo/app/build", "--content", "cargo build"]);
    for agent in ["copilot", "vscode-copilot"] {
        let note = format!("{agent} saw <b>html</b>");
        seed(&home, &["append", "--agent", agent, "--session", "app", "--content", &note]);
    }
    // The store takes empty agent and session names. The page pages past them and opens them.
    seed(&home, &["append", "--agent", "", "--session", "", "--content", "no names"]);
    seed(&home, &["append", "--agent", "copilot", "--session", "", "--content", "no session"]);
    // URLSearchParams writes a space as + and a + as %2B.
    seed(&home, &["append", "--agent", "copilot", "--session", "my app+1", "--content", "plus"]);
    seed(&home, &["fact", "set", "--key", "repo/web/deploy", "--content", "pages from main"]);
    let config = data.parent().unwrap().join("atticdb.json");
    let tenants = serde_json::from_slice::<Value>(&std::fs::read(&config).unwrap()).unwrap();
    let token = tenants["tenants"][0]["token"].as_str().unwrap().to_owned();
    let store = Via::Unix(data.join(atticdb::listen::SOCKET_NAME));
    let studio = Studio::start(&home, &["--no-open"], &[]);

    // The page needs no secret. It holds no data. `/` and every asset it links are the files in
    // `studio/`, each with its content type.
    let page_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("studio");
    let file = |name: &str| std::fs::read_to_string(page_dir.join(name)).unwrap();
    let html = file("index.html");
    let mut pages = vec![("/".to_owned(), "index.html".to_owned())];
    pages.extend(links(&html).into_iter().map(|link| (format!("/{link}"), link)));
    let names: Vec<&str> = pages.iter().map(|(_, name)| name.as_str()).collect();
    assert!(names.contains(&"app.js") && names.contains(&"app.css"), "{names:?}");
    for (target, name) in &pages {
        let kind = match name.rsplit_once('.').map(|(_, ext)| ext) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            _ => panic!("{target}: no content type for {name}"),
        };
        let reply = studio.get(target, false);
        assert_eq!((reply.status, reply.header("Content-Type")), (200, Some(kind)), "{target}");
        assert!(reply.body == file(name), "{target}: not {name}");
    }
    // Store text reaches the page only as text: no API that parses markup or runs a string,
    // and nothing inline for the CSP to refuse.
    let js = file("app.js");
    for api in
        ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval(", "new Function"]
    {
        assert!(!js.contains(api), "app.js uses {api}");
    }
    for timer in ["setTimeout(", "setInterval("] {
        for (at, _) in js.match_indices(timer) {
            let arg = js[at + timer.len()..].trim_start();
            assert!(!arg.starts_with(['"', '\'', '`']), "app.js passes a string to {timer}");
        }
    }
    let lower = html.to_ascii_lowercase();
    for (at, _) in lower.match_indices("<script") {
        let tag = &lower[at..at + lower[at..].find('>').unwrap()];
        assert!(tag.contains(" src="), "index.html has an inline script: {tag}");
    }
    assert!(!lower.contains("<style") && !lower.contains("style="), "index.html styles inline");
    // An inline handler is `on`, letters and `=`, after a space, a slash or a quote.
    for (at, _) in lower.match_indices("on") {
        let after = lower[..at].ends_with(|c: char| c.is_ascii_whitespace() || "/\"'".contains(c));
        let rest = &lower[at + 2..];
        let name = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_lowercase()).len();
        let handler = after && name > 0 && rest[name..].trim_start().starts_with('=');
        assert!(!handler, "index.html has an inline event handler: {}", &html[at..at + 2 + name]);
    }
    assert!(!lower.contains("javascript:"), "index.html has a javascript: URL");

    // Each read gets the store's own status and body, a 400 included.
    let reads = [
        "/v1/sessions",
        "/v1/records?limit=2",
        "/v1/window?agent=copilot&session=app&last=5",
        "/v1/search?q=html",
        "/v1/facts/list?prefix=user/",
        "/v1/facts/get?key=user%2Fstyle",
        "/v1/facts/history?key=user/style",
        "/v1/sessions?bogus=1",
    ];
    for target in reads {
        let expected = store.call(Some("localhost"), Some(&token), "GET", target, None);
        let reply = studio.get(&format!("/api{target}"), true);
        assert_eq!((reply.status, reply.json()), expected, "{target}");
        assert_eq!(reply.header("Content-Type"), Some("application/json"), "{target}");
    }
    let (_, stats) = store.call(Some("localhost"), None, "GET", "/v1/stats", None);
    let reply = studio.get("/api/v1/stats", true).json();
    let fields = |stats: &Value| (stats["instance"].clone(), stats["durable_lsn"].clone());
    assert_eq!(fields(&reply), fields(&stats), "{reply}");
    let metrics = studio.get("/api/metrics", true);
    let kind = Some("text/plain; version=0.0.4");
    assert_eq!((metrics.status, metrics.header("Content-Type")), (200, kind));
    assert!(metrics.body.contains("\natticdb_durable_lsn "), "{}", metrics.body);

    // The requests app.js builds, as URLSearchParams writes them. Each answers 200 with a
    // non-empty list, and every item holds the fields the page reads.
    let fields = |list: &str, item: &Value| -> &'static [&'static str] {
        match (list, item["kind"].as_str()) {
            ("sessions", _) => &["agent", "session", "turns"],
            ("turns", _) => &["seq", "lsn", "commit_time_ns", "role", "content", "meta"],
            ("revisions", _) => &["lsn", "commit_time_ns", "agent", "deleted", "content", "meta"],
            ("facts", _) => &["key", "lsn", "commit_time_ns", "agent", "content", "meta"],
            (_, Some("turn")) => {
                &["kind", "lsn", "commit_time_ns", "agent", "session", "seq", "role", "content"]
            }
            (_, Some("fact")) => {
                &["kind", "lsn", "commit_time_ns", "agent", "key", "deleted", "content"]
            }
            _ => panic!("{list}: no kind in {item}"),
        }
    };
    let asked = |target: &str, list: &str| -> Value {
        let reply = studio.get(target, true);
        assert_eq!(reply.status, 200, "{target}: {}", reply.body);
        let body = reply.json();
        let top: &[&str] = match list {
            "sessions" => &["next", "total"],
            "turns" | "revisions" => &["truncated"],
            _ => &["next"],
        };
        for name in top {
            assert!(body.get(name).is_some(), "{target}: no {name} in {body}");
        }
        if list == "sessions" {
            let total = &body["total"];
            assert!(total["sessions"].is_u64() && total["turns"].is_u64(), "{target}: {body}");
        }
        let items = body[list].as_array().unwrap_or_else(|| panic!("{target}: {body}"));
        assert!(!items.is_empty(), "{target}: no {list}");
        for item in items {
            for name in fields(list, item) {
                assert!(item.get(name).is_some(), "{target}: no {name} in {item}");
            }
        }
        body
    };
    let first = |body: &Value, list: &str, field: &str| body[list][0][field].clone();
    // Sessions sort by the bytes of agent, then session, so ("", "") comes first. The cursor past
    // it is 2 empty values, and both must reach the store.
    let page = asked("/api/v1/sessions?limit=1000", "sessions");
    assert_eq!(
        (first(&page, "sessions", "agent"), first(&page, "sessions", "session")),
        (json!(""), json!(""))
    );
    asked("/api/v1/sessions?limit=1", "sessions");
    let page = asked("/api/v1/sessions?limit=1000&after_agent=&after_session=", "sessions");
    assert_eq!(
        (first(&page, "sessions", "agent"), first(&page, "sessions", "session")),
        (json!("copilot"), json!(""))
    );
    let page = asked("/api/v1/sessions?limit=1000&after_agent=copilot&after_session=", "sessions");
    assert_eq!(first(&page, "sessions", "session"), "app");
    let page =
        asked("/api/v1/sessions?limit=1000&after_agent=copilot&after_session=app", "sessions");
    assert_eq!(first(&page, "sessions", "session"), "my app+1");
    // The Overview and Chat start, then the log by kind. `before` takes an LSN, as `next` gives.
    let page = asked("/api/v1/records?limit=8&max_bytes=8388608", "records");
    let before = first(&page, "records", "lsn");
    asked(&format!("/api/v1/records?before={before}&limit=8&max_bytes=8388608"), "records");
    asked("/api/v1/records?limit=50&max_bytes=8388608", "records");
    let page = asked("/api/v1/records?limit=50&kind=fact&max_bytes=8388608", "records");
    let before = first(&page, "records", "lsn");
    let target = format!("/api/v1/records?before={before}&limit=50&kind=fact&max_bytes=8388608");
    let page = asked(&target, "records");
    assert!(page["records"].as_array().unwrap().iter().all(|record| record["kind"] == "fact"));
    // A session's newest turns, then older ones from a seq, empty names and + included.
    asked("/api/v1/window?agent=copilot&session=app&last=50&max_bytes=8388608", "turns");
    let target = "/api/v1/window?agent=copilot&session=app&since_seq=1&limit=1&max_bytes=8388608";
    asked(target, "turns");
    let page = asked("/api/v1/window?agent=&session=&last=50&max_bytes=8388608", "turns");
    assert_eq!(first(&page, "turns", "content"), "no names");
    let target = "/api/v1/window?agent=copilot&session=&since_seq=1&limit=1&max_bytes=8388608";
    assert_eq!(first(&asked(target, "turns"), "turns", "content"), "no session");
    let target = "/api/v1/window?agent=copilot&session=my+app%2B1&last=50&max_bytes=8388608";
    assert_eq!(first(&asked(target, "turns"), "turns", "content"), "plus");
    // A fact, every fact, the repos for Chat start, and a fact's neighbours.
    asked("/api/v1/facts/history?key=user%2Fstyle&max_bytes=8388608", "revisions");
    asked("/api/v1/facts/list?limit=1000", "facts");
    asked("/api/v1/facts/list?after=repo%2Fapp%2Fbuild&limit=1000", "facts");
    asked("/api/v1/facts/list?prefix=repo%2F&limit=1000", "facts");
    // Chat start goes on past a repo it knows: its prefix and U+10FFFF.
    let target = "/api/v1/facts/list?prefix=repo%2F&after=repo%2Fapp%2F%F4%8F%BF%BF&limit=1000";
    assert_eq!(first(&asked(target, "facts"), "facts", "key"), "repo/web/deploy");
    asked("/api/v1/facts/list?prefix=user%2F&limit=8", "facts");
    asked("/api/v1/facts/list?limit=8", "facts");
    let page = asked("/api/v1/search?q=html&limit=50&max_bytes=8388608", "hits");
    let before = first(&page, "hits", "lsn");
    asked(&format!("/api/v1/search?q=html&before={before}&limit=50&max_bytes=8388608"), "hits");

    // Checks run in order: Host, Origin, method, then the secret for /api/.
    let (_, sessions) = store.call(Some("localhost"), Some(&token), "GET", "/v1/sessions", None);
    let (port, secret) = (studio.port, &studio.secret);
    let (host, key) =
        (format!("Host: 127.0.0.1:{port}\r\n"), format!("X-Atticdb-Studio: {secret}\r\n"));
    let (ok, own) = (format!("{host}{key}"), format!("http://127.0.0.1:{port}"));
    let (wrong, short) = ("0".repeat(32), &secret[..31]);
    let request = |method: &str, path: &str, headers: &str| {
        format!("{method} {path} HTTP/1.1\r\n{headers}\r\n")
    };
    let get = |headers: &str| request("GET", "/api/v1/sessions", headers);
    let (unauthorized, forbidden) = ("unauthorized", "forbidden");
    // (case, request, status, error)
    let rows = [
        ("no secret", get(&host), 401, unauthorized),
        ("a wrong secret", get(&format!("{host}X-Atticdb-Studio: {wrong}\r\n")), 401, unauthorized),
        ("a short secret", get(&format!("{host}X-Atticdb-Studio: {short}\r\n")), 401, unauthorized),
        ("the secret twice", get(&format!("{ok}{key}")), 401, unauthorized),
        ("the secret", get(&ok), 200, ""),
        ("its own Origin", get(&format!("{ok}Origin: {own}\r\n")), 200, ""),
        ("2 requests", get(&ok) + &get(&ok), 200, ""),
        ("Host evil.example", get(&format!("Host: evil.example\r\n{key}")), 403, forbidden),
        ("Host localhost", get(&format!("Host: localhost:{port}\r\n{key}")), 403, forbidden),
        ("no Host", get(&key), 403, forbidden),
        ("a foreign Origin", get(&format!("{ok}Origin: http://evil.example\r\n")), 403, forbidden),
        ("POST", request("POST", "/api/v1/sessions", &ok), 405, "method_not_allowed"),
        ("a body", get(&format!("{ok}Content-Length: 2\r\n\r\n{{}}")), 400, "bad_request"),
        ("append", request("GET", "/api/v1/append", &ok), 404, "not_found"),
        ("facts/put", request("GET", "/api/v1/facts/put", &ok), 404, "not_found"),
        // Without --allow-forget, the forget route is a path like any other.
        ("GET forget", request("GET", "/api/forget", &ok), 404, "not_found"),
        ("POST forget", request("POST", "/api/forget", &ok), 405, "method_not_allowed"),
        ("not /api/", request("GET", "/v1/sessions", &ok), 404, "not_found"),
        ("block without repo", request("GET", "/api/block", &ok), 400, "invalid_input"),
    ];
    for (case, request, status, error) in rows {
        let reply = studio.send(case, &request);
        let body = reply.json();
        let found = (reply.status, body["error"].as_str().unwrap_or_default());
        assert_eq!(found, (status, error), "{case}: {body}");
        if status == 200 {
            assert_eq!(body, sessions, "{case}: the store's body");
        }
    }

    // The block is the one the hook prints for a chat in that repo.
    let (out, _) = hook(&home, &cwd(&home.join("app")), 0, &[]);
    let block = serde_json::from_str::<Value>(&out).unwrap()["additionalContext"].clone();
    let chars = block.as_str().unwrap().chars().count();
    let reply = studio.get("/api/block?repo=app", true);
    let expected = json!({"repo": "app", "block": block, "chars": chars, "max": 6000});
    assert_eq!((reply.status, reply.json()), (200, expected));
    let version = atticdb::VERSION;
    let info = json!({"version": version, "tenant": "me", "store": "home", "home": data.parent(),
                      "forget": false});
    assert_eq!(studio.get("/api/info", true).json(), info);

    // On the home's data directory: the same store, but no block, as the hook reads the home.
    // It can forget too. That runs below.
    let (config, data_flag) = (config.to_str().unwrap(), data.to_str().unwrap());
    let flags = [
        "--no-open",
        "--config",
        config,
        "--tenant",
        "me",
        "--data-dir",
        data_flag,
        "--allow-forget",
    ];
    let by_dir = Studio::start(&home, &flags, &[]);
    let reply = by_dir.get("/api/block?repo=app", true);
    assert_eq!((reply.status, reply.json()), (404, json!({"error": "not_home"})));
    let info = json!({"version": version, "tenant": "me", "store": "data_dir", "home": null,
                      "forget": true});
    assert_eq!(by_dir.get("/api/info", true).json(), info);
    assert_eq!(by_dir.get("/api/v1/sessions", true).json(), sessions);

    // A token file the store refuses. Reads get 502, so the page never takes the store's 401
    // for its own wrong secret. /api/info needs no store and still answers.
    let wrong = private_file(&dir, "wrong", TOKEN_B);
    let refused = store.call(Some("localhost"), Some(TOKEN_B), "GET", "/v1/records", None);
    assert_eq!(refused.0, 401, "the store refuses that token: {}", refused.1);
    let flags = ["--no-open", "--data-dir", data_flag, "--token-file", &wrong];
    let bad = Studio::start(&home, &flags, &[]);
    let reply = bad.get("/api/v1/records", true);
    let message = "the store refused the studio's token";
    let unauthorized = json!({"error": "store_unauthorized", "message": message});
    assert_eq!((reply.status, reply.json()), (502, unauthorized));
    let info = json!({"version": version, "tenant": null, "store": "data_dir", "home": null,
                      "forget": false});
    let reply = bad.get("/api/info", true);
    assert_eq!((reply.status, reply.json()), (200, info));

    // On a --url with a token file, in a new home. ATTICDB_TOKEN holds the token too, but the
    // file wins. A fake opener records the token it got and its arguments, then fails. The
    // studio says so and keeps serving.
    let (other, bin) = (dir.0.join("u"), dir.0.join("bin"));
    std::fs::create_dir_all(&other).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let (opener, args) =
        (bin.join(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }), bin.join("args"));
    let tmp = args.with_extension("tmp");
    let (tmp, args_text) = (tmp.display(), args.display());
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"${{ATTICDB_TOKEN-unset}}\" $# \"$@\" > '{tmp}' \
         && mv '{tmp}' '{args_text}'\nexit 1\n"
    );
    std::fs::write(&opener, script).unwrap();
    chmod(&opener, 0o700);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = [bin.clone()].into_iter().chain(std::env::split_paths(&path));
    let path = std::env::join_paths(path).unwrap();
    let link = dir.0.join("l.sock");
    std::os::unix::fs::symlink(data.join(atticdb::listen::SOCKET_NAME), &link).unwrap();
    let url = format!("unix://{}", link.display());
    let token_file = private_file(&dir, "token", &token);
    let env = [("PATH", path.as_os_str()), ("ATTICDB_TOKEN", OsStr::new(&token))];
    let flags = ["--url", &url, "--token-file", &token_file, "--allow-forget"];
    let by_url = Studio::start(&other, &flags, &env);
    wait_for("the opener", || args.exists());
    wait_for("the note", || by_url.process.stderr().contains("cannot open a browser"));
    let page = data_dir(&other).parent().unwrap().join("studio.html");
    let opened = format!("unset\n1\n{}\n", page.display());
    assert_eq!(std::fs::read_to_string(&args).unwrap(), opened, "no token, only the file");
    // A Snap browser can fail on the hidden folder while xdg-open exits 0.
    let hint = "atticdb studio: if the browser shows an error, open the link above.";
    assert_eq!(by_url.process.stderr().contains(hint), cfg!(target_os = "linux"), "the hint");
    let html = std::fs::read_to_string(&page).unwrap();
    let link_text = format!("http://127.0.0.1:{}/#k={}", by_url.port, by_url.secret);
    assert!(html.contains(&format!("content=\"0;url={link_text}\"")), "{html}");
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&page), mode(page.parent().unwrap())), (0o600, 0o700));
    let names: Vec<String> = files(page.parent().unwrap()).into_keys().collect();
    assert_eq!(names, ["studio.html"], "no config and no temp file");
    let info =
        json!({"version": version, "tenant": null, "store": "url", "home": null, "forget": true});
    assert_eq!(by_url.get("/api/info", true).json(), info);
    assert_eq!(by_url.get("/api/v1/sessions", true).json(), sessions);
    // Once the store's path is gone, reads answer 502, logged without the query.
    std::fs::remove_file(&link).unwrap();
    let reply = by_url.get("/api/v1/sessions?limit=1", true);
    let body = reply.json();
    assert_eq!((reply.status, &body["error"]), (502, &json!("store_unreachable")), "{body}");
    assert!(body["message"].as_str().is_some_and(|text| !text.is_empty()), "{body}");
    let logged = "atticdb studio: GET /api/v1/sessions 502 ";
    wait_for("the 502 line", || by_url.process.stderr().contains(logged));
    // A forget then sends nothing. Every item is not_sent, so the page keeps it for a retry.
    let item = |n: usize| {
        let session = json!({"agent": "copilot", "session": "app"});
        json!({"idem_key": format!("{n:032x}"), "session": session, "up_to_lsn": 1})
    };
    let reply =
        by_url.forget("no store", &json!({"confirm": "hide", "forgets": [item(1), item(2)]}));
    let results = reply.json()["results"].clone();
    let outcomes: Vec<_> = results.as_array().unwrap().iter().map(|r| &r["outcome"]).collect();
    assert_eq!((reply.status, outcomes), (200, vec![&json!("not_sent"); 2]), "{results}");
    assert_eq!(results[0]["error"], "store_unreachable", "{results}");
    let logged =
        "atticdb studio: forget 2 sessions: 0 committed, 0 not committed, 0 unknown, 2 not sent";
    wait_for("the not-sent line", || by_url.process.stderr().contains(logged));
    // With no store at start, it exits 1 and prints no link.
    let missing = format!("unix://{}", dir.0.join("missing.sock").display());
    let flags = ["--no-open", "--url", &missing, "--token-file", &token_file];
    let mut process = Process::start(&mut command(&other, &flags, &[]));
    let (status, output) = process.wait();
    assert_eq!(status.code(), Some(1), "{output}");
    let refused = process.stderr().starts_with("atticdb: cannot reach the store: ");
    assert!(process.stdout().is_empty() && refused, "{output}");

    // With --allow-forget, 1 write route. Checks run in order: Host, Origin, method,
    // Sec-Fetch-Site, the secret, then Content-Type. Then the body.
    let forgetful = Studio::start(&home, &["--no-open", "--allow-forget"], &[]);
    let info = json!({"version": version, "tenant": "me", "store": "home", "home": data.parent(),
                      "forget": true});
    assert_eq!(forgetful.get("/api/info", true).json(), info);
    let port = forgetful.port;
    let (host, key) = (
        format!("Host: 127.0.0.1:{port}\r\n"),
        format!("X-Atticdb-Studio: {}\r\n", forgetful.secret),
    );
    let (own, typed) =
        (format!("Origin: http://127.0.0.1:{port}\r\n"), "Content-Type: application/json\r\n");
    let all = format!("{host}{key}{own}Sec-Fetch-Site: same-origin\r\n{typed}");
    let post = |headers: &str, body: &Value| {
        let body = body.to_string();
        format!(
            "POST /api/forget HTTP/1.1\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let hex = |n: usize| format!("{n:032x}");
    let item = |n: usize, agent: &str, lsn: Value| {
        let session = json!({"agent": agent, "session": "app"});
        json!({"idem_key": hex(n), "session": session, "up_to_lsn": lsn})
    };
    let hide = |items: Value| json!({"confirm": "hide", "forgets": items});
    let good = hide(json!([item(1, "copilot", json!(1))]));
    let no_lsn = json!({"idem_key": hex(1), "session": {"agent": "copilot", "session": "app"}});
    let many: Vec<Value> = (0..101).map(|n| item(n, "copilot", json!(1))).collect();
    let mixed =
        json!([item(1, "copilot", json!(1)), {"idem_key": hex(2), "key": "k", "up_to_lsn": 1}]);
    let (invalid, unsupported) = ("invalid_input", "unsupported_media_type");
    let (foreign, cross) = ("Origin: http://evil.example\r\n", "Sec-Fetch-Site: cross-site\r\n");
    let (plain, form) =
        ("Content-Type: text/plain\r\n", "Content-Type: application/x-www-form-urlencoded\r\n");
    let (no_confirm, mixed) = (json!({"forgets": [item(1, "copilot", json!(1))]}), hide(mixed));
    let big = format!("POST /api/forget HTTP/1.1\r\n{all}Content-Length: 65537\r\n\r\n");
    let (not_allowed, sessions_post) =
        ("method_not_allowed", format!("POST /api/v1/sessions HTTP/1.1\r\n{host}{key}\r\n"));
    // (case, request, status, error)
    let rows = [
        ("no Origin", post(&format!("{host}{key}{typed}"), &good), 403, forbidden),
        ("a foreign Origin", post(&format!("{host}{key}{foreign}{typed}"), &good), 403, forbidden),
        ("Origin twice", post(&format!("{all}{own}"), &good), 403, forbidden),
        ("cross-site", post(&format!("{host}{key}{own}{cross}{typed}"), &good), 403, forbidden),
        ("no secret", post(&format!("{host}{own}{typed}"), &good), 401, "unauthorized"),
        ("text/plain", post(&format!("{host}{key}{own}{plain}"), &good), 415, unsupported),
        ("form-encoded", post(&format!("{host}{key}{own}{form}"), &good), 415, unsupported),
        ("no confirm", post(&all, &no_confirm), 400, invalid),
        ("no up_to_lsn", post(&all, &hide(json!([no_lsn]))), 400, invalid),
        ("up_to_lsn 0", post(&all, &hide(json!([item(1, "copilot", json!(0))]))), 400, invalid),
        ("101 items", post(&all, &hide(json!(many))), 400, invalid),
        ("mixed selectors", post(&all, &mixed), 400, invalid),
        ("over 64 KiB", big, 413, "too_large"),
        ("OPTIONS", format!("OPTIONS /api/forget HTTP/1.1\r\n{all}\r\n"), 405, not_allowed),
        ("GET forget", format!("GET /api/forget HTTP/1.1\r\n{host}{key}\r\n"), 405, not_allowed),
        ("POST a read", sessions_post, 405, not_allowed),
    ];
    for (case, request, status, error) in rows {
        let reply = forgetful.send(case, &request);
        let body = reply.json();
        let found = (reply.status, body["error"].as_str().unwrap_or_default());
        assert_eq!(found, (status, error), "{case}: {body}");
        assert!(
            !reply.head.to_ascii_lowercase().contains("access-control-"),
            "{case}: no CORS answer"
        );
    }

    // The page pins each forget to the as_of_lsn it loaded at. A turn written after it stays.
    let page = forgetful.get("/api/v1/sessions?limit=1000", true).json();
    let pin = page["as_of_lsn"].as_u64().unwrap();
    let late = json!({"agent": "copilot", "session": "app", "idem_key": "late",
                      "turns": [{"role": "assistant", "content": LATE}]});
    let (status, appended) =
        store.call(Some("localhost"), Some(&token), "POST", "/v1/append", Some(&late));
    assert!(status == 200 && appended["first_lsn"].as_u64().unwrap() > pin, "{appended}");
    let both = hide(json!([item(7, "copilot", json!(pin)), item(8, "vscode-copilot", json!(pin))]));
    let mut receipts = Vec::new();
    for (case, retried) in [("bulk forget", false), ("its retry", true)] {
        let reply = forgetful.forget(case, &both);
        assert_eq!(reply.status, 200, "{case}: {}", reply.body);
        let results = reply.json()["results"].as_array().unwrap().clone();
        assert_eq!(results.len(), 2, "{case}: {results:?}");
        for result in &results {
            let found = (&result["outcome"], &result["deduplicated"], &result["cutoff"]);
            assert_eq!(found, (&json!("committed"), &json!(retried), &json!(pin)), "{case}");
        }
        receipts
            .push(results.iter().map(|r| (r["txn"].clone(), r["lsn"].clone())).collect::<Vec<_>>());
    }
    assert_eq!(receipts[0], receipts[1], "the same keys give the same forgets");
    let logged =
        "atticdb studio: forget 2 sessions: 2 committed, 0 not committed, 0 unknown, 0 not sent\n";
    wait_for("the forget lines", || forgetful.process.stderr().matches(logged).count() == 2);
    // Re-fetched as the page does: the window, the sessions, the records and the block.
    let window = "/api/v1/window?agent=copilot&session=app&last=50&max_bytes=8388608";
    let turns = forgetful.get(window, true).json()["turns"].clone();
    let lsn = turns[0]["lsn"].clone();
    assert!(lsn.as_u64().unwrap() <= pin, "{turns}");
    assert_eq!(turns[0], json!({"seq": 1, "lsn": lsn, "redacted": true}));
    let shown = (&turns[1]["seq"], &turns[1]["content"], turns.as_array().unwrap().len());
    assert_eq!(shown, (&json!(2), &json!(LATE), 2), "{turns}");
    let page = forgetful.get("/api/v1/sessions?limit=1000", true).json();
    for session in page["sessions"].as_array().unwrap() {
        let forgotten = if session["session"] == "app" { json!(pin) } else { Value::Null };
        assert_eq!(session["forgotten_up_to"], forgotten, "{session}");
    }
    let records = forgetful.get("/api/v1/records?limit=50&max_bytes=8388608", true).json();
    let in_app = records["records"].as_array().unwrap().iter().filter(|r| r["session"] == "app");
    let (mut hidden, mut later) = (0, 0);
    for record in in_app {
        if record["lsn"].as_u64().unwrap() > pin {
            assert_eq!(record["content"], LATE);
            later += 1;
            continue;
        }
        let names = ["kind", "agent", "session", "seq", "lsn", "redacted"];
        let placeholder: serde_json::Map<String, Value> =
            names.iter().map(|name| (name.to_string(), record[name].clone())).collect();
        assert_eq!((record, &record["redacted"]), (&Value::Object(placeholder), &json!(true)));
        hidden += 1;
    }
    assert_eq!((hidden, later), (2, 1), "{records}");
    let (out, _) = hook(&home, &cwd(&home.join("app")), 0, &[]);
    let block = serde_json::from_str::<Value>(&out).unwrap()["additionalContext"].clone();
    let text = block.as_str().unwrap();
    assert!(text.contains(LATE) && !text.contains("saw <b>html</b>"), "{text}");
    assert_eq!(forgetful.get("/api/block?repo=app", true).json()["block"], block);
    // A fact key, from the data-dir studio, pinned to the newest revision shown.
    let history = "/api/v1/facts/history?key=repo%2Fweb%2Fdeploy&max_bytes=8388608";
    let newest = by_dir.get(history, true).json()["revisions"][0]["lsn"].clone();
    let one = json!([{"idem_key": hex(9), "key": "repo/web/deploy", "up_to_lsn": newest}]);
    let reply = by_dir.forget("a fact key", &hide(one));
    let result = reply.json()["results"][0].clone();
    assert_eq!(
        (reply.status, &result["outcome"], &result["cutoff"]),
        (200, &json!("committed"), &newest)
    );
    let current = by_dir.get("/api/v1/facts/get?key=repo%2Fweb%2Fdeploy", true).json();
    assert_eq!(current, json!({"fact": null}));
    let revisions = by_dir.get(history, true).json()["revisions"].clone();
    assert_eq!(revisions, json!([{"lsn": newest, "redacted": true}]));
    let logged =
        "atticdb studio: forget 1 fact key: 1 committed, 0 not committed, 0 unknown, 0 not sent\n";
    wait_for("the key line", || by_dir.process.stderr().contains(logged));

    // 16 idle connections fill the cap, so the next is refused on accept. It sends nothing:
    // closing over unread bytes makes Linux reset it before the answer is read.
    let addr = SocketAddr::from(([127, 0, 0, 1], studio.port));
    let mut idle: Vec<TcpStream> = (0..16).map(|_| TcpStream::connect(addr).unwrap()).collect();
    let busy = loop {
        let mut stream = TcpStream::connect(addr).unwrap();
        // Fails on macOS once the studio has closed. The read then returns at once.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut wire = String::new();
        if stream.read_to_string(&mut wire).is_ok() && !wire.is_empty() {
            break wire;
        }
        // A connection from an earlier row was still closing, so this one got a slot.
        idle.push(stream);
        assert!(idle.len() < 32, "no busy answer");
    };
    let reply = Reply::parse("busy", &busy);
    assert_eq!((reply.status, reply.json()), (503, json!({"error": "busy"})));
    drop(idle);

    // The token stays in the studio. The secret is only in the link on stdout.
    for (studio, forget) in
        [(&studio, false), (&by_dir, true), (&bad, false), (&by_url, true), (&forgetful, true)]
    {
        for wire in studio.wires.lock().unwrap().iter() {
            assert!(!wire.contains(&token), "the token reached the page: {wire}");
        }
        let (port, secret) = (studio.port, &studio.secret);
        let on = if forget { "atticdb studio: forget is on (--allow-forget).\n" } else { "" };
        let out = format!(
            "atticdb studio: http://127.0.0.1:{port}/#k={secret}\n{on}Press Ctrl-C to stop.\n"
        );
        assert_eq!(studio.process.stdout(), out);
        // Forget lines hold counts, never a name.
        let err = studio.process.stderr();
        let named = ["limit=1", "copilot", "deploy"].iter().any(|text| err.contains(text));
        assert!(!err.contains(secret) && !err.contains(&token) && !named, "{err}");
    }
    assert!(!data.parent().unwrap().join("studio.html").exists(), "--no-open writes no page");
}

/// A store that takes a read and never answers. 1 deadline of 10 s covers connect and
/// exchange, then the read gets 504. The studio serves other requests meanwhile.
#[test]
#[ignore = "waits for the 10 s store deadline"]
fn studio_answers_504_when_the_store_stalls() {
    let dir = Dir::new();
    let socket = dir.0.join("s.sock");
    let reads = slow_store(&socket, None);
    let (url, token) = (format!("unix://{}", socket.display()), private_file(&dir, "t", TOKEN_A));
    let studio = Studio::start(&dir.0, &["--no-open", "--url", &url, "--token-file", &token], &[]);
    let (reply, elapsed) = std::thread::scope(|scope| {
        let read = scope.spawn(|| {
            let started = Instant::now();
            (studio.get("/api/v1/sessions", true), started.elapsed())
        });
        wait_for("the read reaches the store", || reads.load(Ordering::SeqCst) == 1);
        let info = studio.get("/api/info", true).json();
        let answered = (&info["store"], read.is_finished());
        assert_eq!(answered, (&json!("url"), false), "/api/info answers during the stall: {info}");
        read.join().unwrap()
    });
    assert_eq!((reply.status, reply.body.as_str()), (504, r#"{"error":"store_timeout"}"#));
    eprintln!("504 after {elapsed:?}");
    let window = Duration::from_secs(10)..Duration::from_secs(12);
    assert!(window.contains(&elapsed), "504 after {elapsed:?}");
}
