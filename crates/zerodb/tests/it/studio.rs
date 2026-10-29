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

/// A running `zerodb studio`, killed on drop.
struct Studio {
    process: Process,
    port: u16,
    secret: String,
    /// Every reply, as it came off the wire.
    wires: Mutex<Vec<String>>,
}

impl Studio {
    /// Runs `zerodb studio` and waits for its link.
    fn start(home: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Self {
        let mut process = Process::start(&mut command(home, args, env));
        let deadline = Instant::now() + Duration::from_secs(60);
        let link = loop {
            let out = process.stdout();
            if let Some(link) = out.lines().find_map(|line| line.strip_prefix("zerodb studio: ")) {
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
        let key = if secret { format!("X-Zerodb-Studio: {}\r\n", self.secret) } else { "".into() };
        let host = format!("Host: 127.0.0.1:{}\r\n", self.port);
        self.send(target, &format!("GET {target} HTTP/1.1\r\n{host}{key}\r\n"))
    }
}

/// `zerodb studio` with `HOME` set to `home`, and `env` added.
fn command(home: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zerodb"));
    command.arg("studio").args(args).current_dir(home).env("HOME", home).stdin(Stdio::null());
    for var in ["COPILOT_HOME", "XDG_DATA_HOME", "ZERODB_TOKEN"] {
        command.env_remove(var);
    }
    command.envs(env.iter().copied());
    command
}

/// Studios on the home, on its data directory and on a `--url`. Each serves its page, forwards
/// reads with the token it holds, and refuses everything else, with the same headers.
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
    let config = data.parent().unwrap().join("zerodb.json");
    let tenants = serde_json::from_slice::<Value>(&std::fs::read(&config).unwrap()).unwrap();
    let token = tenants["tenants"][0]["token"].as_str().unwrap().to_owned();
    let store = Via::Unix(data.join(zerodb::listen::SOCKET_NAME));
    let studio = Studio::start(&home, &["--no-open"], &[]);

    // The page needs no secret. It holds no data.
    let pages = [
        ("/", "text/html; charset=utf-8", r#"<script src="/app.js""#),
        ("/app.js", "text/javascript; charset=utf-8", "X-Zerodb-Studio"),
        ("/app.css", "text/css; charset=utf-8", "font-family"),
    ];
    for (target, kind, holds) in pages {
        let reply = studio.get(target, false);
        assert_eq!((reply.status, reply.header("Content-Type")), (200, Some(kind)), "{target}");
        assert!(reply.body.contains(holds), "{target}: {}", reply.body);
    }

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
    assert!(metrics.body.contains("\nzerodb_durable_lsn "), "{}", metrics.body);

    // Checks run in order: Host, Origin, method, then the secret for /api/.
    let (_, sessions) = store.call(Some("localhost"), Some(&token), "GET", "/v1/sessions", None);
    let (port, secret) = (studio.port, &studio.secret);
    let (host, key) =
        (format!("Host: 127.0.0.1:{port}\r\n"), format!("X-Zerodb-Studio: {secret}\r\n"));
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
        ("a wrong secret", get(&format!("{host}X-Zerodb-Studio: {wrong}\r\n")), 401, unauthorized),
        ("a short secret", get(&format!("{host}X-Zerodb-Studio: {short}\r\n")), 401, unauthorized),
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
    let (out, _) = hook(&home, &cwd(&home.join("app")), 0);
    let block = serde_json::from_str::<Value>(&out).unwrap()["additionalContext"].clone();
    let chars = block.as_str().unwrap().chars().count();
    let reply = studio.get("/api/block?repo=app", true);
    let expected = json!({"repo": "app", "block": block, "chars": chars, "max": 6000});
    assert_eq!((reply.status, reply.json()), (200, expected));
    let version = env!("CARGO_PKG_VERSION");
    let info = json!({"version": version, "tenant": "me", "store": "home", "home": data.parent()});
    assert_eq!(studio.get("/api/info", true).json(), info);

    // On the home's data directory: the same store, but no block, as the hook reads the home.
    let (config, data_flag) = (config.to_str().unwrap(), data.to_str().unwrap());
    let flags = ["--no-open", "--config", config, "--tenant", "me", "--data-dir", data_flag];
    let by_dir = Studio::start(&home, &flags, &[]);
    let reply = by_dir.get("/api/block?repo=app", true);
    assert_eq!((reply.status, reply.json()), (404, json!({"error": "not_home"})));
    let info = json!({"version": version, "tenant": "me", "store": "data_dir", "home": null});
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
    let info = json!({"version": version, "tenant": null, "store": "data_dir", "home": null});
    let reply = bad.get("/api/info", true);
    assert_eq!((reply.status, reply.json()), (200, info));

    // On a --url with a token file, in a new home. ZERODB_TOKEN holds the token too, but the
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
        "#!/bin/sh\nprintf '%s\\n' \"${{ZERODB_TOKEN-unset}}\" $# \"$@\" > '{tmp}' \
         && mv '{tmp}' '{args_text}'\nexit 1\n"
    );
    std::fs::write(&opener, script).unwrap();
    chmod(&opener, 0o700);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = [bin.clone()].into_iter().chain(std::env::split_paths(&path));
    let path = std::env::join_paths(path).unwrap();
    let link = dir.0.join("l.sock");
    std::os::unix::fs::symlink(data.join(zerodb::listen::SOCKET_NAME), &link).unwrap();
    let url = format!("unix://{}", link.display());
    let token_file = private_file(&dir, "token", &token);
    let env = [("PATH", path.as_os_str()), ("ZERODB_TOKEN", OsStr::new(&token))];
    let by_url = Studio::start(&other, &["--url", &url, "--token-file", &token_file], &env);
    wait_for("the opener", || args.exists());
    wait_for("the note", || by_url.process.stderr().contains("cannot open a browser"));
    let page = data_dir(&other).parent().unwrap().join("studio.html");
    let opened = format!("unset\n1\n{}\n", page.display());
    assert_eq!(std::fs::read_to_string(&args).unwrap(), opened, "no token, only the file");
    // A Snap browser can fail on the hidden folder while xdg-open exits 0.
    let hint = "zerodb studio: if the browser shows an error, open the link above.";
    assert_eq!(by_url.process.stderr().contains(hint), cfg!(target_os = "linux"), "the hint");
    let html = std::fs::read_to_string(&page).unwrap();
    let link_text = format!("http://127.0.0.1:{}/#k={}", by_url.port, by_url.secret);
    assert!(html.contains(&format!("content=\"0;url={link_text}\"")), "{html}");
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&page), mode(page.parent().unwrap())), (0o600, 0o700));
    let names: Vec<String> = files(page.parent().unwrap()).into_keys().collect();
    assert_eq!(names, ["studio.html"], "no config and no temp file");
    let info = json!({"version": version, "tenant": null, "store": "url", "home": null});
    assert_eq!(by_url.get("/api/info", true).json(), info);
    assert_eq!(by_url.get("/api/v1/sessions", true).json(), sessions);
    // Once the store's path is gone, reads answer 502, logged without the query.
    std::fs::remove_file(&link).unwrap();
    let reply = by_url.get("/api/v1/sessions?limit=1", true);
    let body = reply.json();
    assert_eq!((reply.status, &body["error"]), (502, &json!("store_unreachable")), "{body}");
    assert!(body["message"].as_str().is_some_and(|text| !text.is_empty()), "{body}");
    let logged = "zerodb studio: GET /api/v1/sessions 502 ";
    wait_for("the 502 line", || by_url.process.stderr().contains(logged));
    // With no store at start, it exits 1 and prints no link.
    let missing = format!("unix://{}", dir.0.join("missing.sock").display());
    let flags = ["--no-open", "--url", &missing, "--token-file", &token_file];
    let mut process = Process::start(&mut command(&other, &flags, &[]));
    let (status, output) = process.wait();
    assert_eq!(status.code(), Some(1), "{output}");
    let refused = process.stderr().starts_with("zerodb: cannot reach the store: ");
    assert!(process.stdout().is_empty() && refused, "{output}");

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
    for studio in [&studio, &by_dir, &bad, &by_url] {
        for wire in studio.wires.lock().unwrap().iter() {
            assert!(!wire.contains(&token), "the token reached the page: {wire}");
        }
        let (port, secret) = (studio.port, &studio.secret);
        let out =
            format!("zerodb studio: http://127.0.0.1:{port}/#k={secret}\nPress Ctrl-C to stop.\n");
        assert_eq!(studio.process.stdout(), out);
        let err = studio.process.stderr();
        assert!(
            !err.contains(secret) && !err.contains(&token) && !err.contains("limit=1"),
            "{err}"
        );
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
