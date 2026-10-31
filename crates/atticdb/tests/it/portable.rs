//! The home flow on every platform. A fresh home starts a server, which the shell verbs and
//! the MCP adapter use. On Windows that server is on loopback TCP. Clients find it through
//! `LOCK`, then check its token proof. The squatter tests are Windows only. Unix discovery
//! never uses `tcp=`.
//!
//! Self-contained, as `support` is Unix only. Every server is stopped by the pid in `LOCK`.
//! On Windows each command gets the test folder as its user profile.

use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// Variables that could lead a test to a real home. No spawn inherits them.
pub(crate) const HOME_VARS: [&str; 10] = [
    "HOME",
    "ATTICDB_HOME",
    "ATTICDB_TOKEN",
    "ZERODB_HOME",
    "ZERODB_TOKEN",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "COPILOT_HOME",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
];
/// Longest wait for 1 command.
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

// A private, short directory, removed on drop. Unix uses `support::Dir`, whose short path
// fits a socket.
#[cfg(unix)]
pub(crate) use crate::support::Dir;

/// A new directory under the temp dir, in the user's profile. Removed on drop.
#[cfg(windows)]
pub struct Dir(pub PathBuf);

#[cfg(windows)]
impl Dir {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name =
            format!("atticdb-it-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

#[cfg(windows)]
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether a process holds `data`'s lock.
pub(crate) fn locked(data: &Path) -> bool {
    File::open(data.join("LOCK")).is_ok_and(|file| file.try_lock().is_err())
}

/// The pid in `data`'s `LOCK`.
#[cfg(windows)]
fn lock_pid(data: &Path) -> Option<u32> {
    let lock = std::fs::read_to_string(data.join("LOCK")).ok()?;
    lock.strip_prefix("pid=")?.split(' ').next()?.parse().ok()
}

/// Stops the servers of these data directories at drop, also when a test fails.
pub(crate) struct Servers(pub(crate) Vec<PathBuf>);

impl Drop for Servers {
    fn drop(&mut self) {
        #[cfg(unix)]
        crate::support::stop_servers(&self.0);
        #[cfg(windows)]
        stop_servers(&self.0);
    }
}

/// Kills the server named in each `LOCK`, by pid, when that pid runs `atticdb.exe`. It stops
/// once no lock was held for 500 ms, or after 15 s.
#[cfg(windows)]
pub(crate) fn stop_servers(dirs: &[PathBuf]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut free_since = Instant::now();
    while Instant::now() < deadline {
        let mut held = false;
        for data in dirs.iter().filter(|data| locked(data)) {
            held = true;
            if let Some(pid) = lock_pid(data).filter(|pid| runs_atticdb(*pid)) {
                let taskkill = system32("taskkill.exe");
                let _ = run_bounded(Command::new(taskkill).args(["/PID", &pid.to_string(), "/F"]));
            }
        }
        if held {
            free_since = Instant::now();
        } else if free_since.elapsed() >= Duration::from_millis(500) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn system32(program: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(root).join("System32").join(program)
}

/// Whether `pid` is an `atticdb.exe` process, by `tasklist`.
#[cfg(windows)]
fn runs_atticdb(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    let mut command = Command::new(system32("tasklist.exe"));
    command.args(["/FI", &filter, "/FO", "CSV", "/NH"]);
    run_bounded(&mut command)
        .is_some_and(|out| out.to_ascii_lowercase().contains("\"atticdb.exe\""))
}

/// Runs a helper for up to 30 s. Returns its stdout when it exits.
#[cfg(windows)]
fn run_bounded(command: &mut Command) -> Option<String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let out = drain(child.stdout.take()?);
    let status = wait(&mut child, Duration::from_secs(30))?;
    let _ = status;
    out.recv_timeout(Duration::from_secs(5)).ok()
}

/// Reads `from` to its end on a thread.
pub(crate) fn drain(mut from: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = from.read_to_end(&mut text);
        let _ = sender.send(String::from_utf8_lossy(&text).into_owned());
    });
    receiver
}

/// Waits up to `limit` for `child`, else kills it and returns `None`.
pub(crate) fn wait(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `atticdb` without [`HOME_VARS`], in `cwd`, with `env` added. On Windows `cwd` is also the
/// user profile, so homes in it pass the profile check.
fn atticdb(cwd: &Path, env: &[(&str, &Path)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_atticdb"));
    for var in HOME_VARS {
        command.env_remove(var);
    }
    command.current_dir(cwd);
    if cfg!(windows) {
        command.env("USERPROFILE", cwd).env("LOCALAPPDATA", cwd.join("AppData").join("Local"));
    }
    for (name, value) in env {
        command.env(name, value);
    }
    command
}

/// Runs `atticdb args` with `env` and returns the exit code, stdout and stderr.
fn run(cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = atticdb(cwd, env);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let (out, err) = (drain(child.stdout.take().unwrap()), drain(child.stderr.take().unwrap()));
    let status = wait(&mut child, COMMAND_TIMEOUT);
    let out = out.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let err = err.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    assert!(status.is_some(), "{args:?} did not exit in time: {out}{err}");
    (status.and_then(|status| status.code()), out, err)
}

/// Runs a verb on the home `home` that must exit 0, and returns what it printed as JSON.
fn verb(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let (code, out, err) = run(cwd, &[("ATTICDB_HOME", home)], args);
    assert_eq!(code, Some(0), "{args:?}: {out}{err}");
    serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("{args:?}: not JSON: {out}"))
}

/// A fresh home: the first verb starts a server, which the others use. Then a new process
/// finds it through `LOCK`.
#[test]
fn a_fresh_home_starts_a_server_for_the_verbs() {
    let dir = Dir::new();
    let home = dir.0.join("home");
    let _servers = Servers(vec![home.join("data")]);
    let env = [("ATTICDB_HOME", home.as_path())];
    let (code, out, err) = run(&dir.0, &env, &["append", "--session", "s", "--content", "hi x"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(err.contains("atticdb: started server pid "), "{err}");
    let printed: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(printed["seq"], 1, "{out}");
    assert!(locked(&home.join("data")), "the server outlives the verb");
    let lock = std::fs::read_to_string(home.join("data/LOCK")).unwrap();
    if cfg!(windows) {
        assert!(lock.contains(" tcp=127.0.0.1:") && !lock.contains("unix="), "{lock}");
    }

    let found = verb(&dir.0, &home, &["search", "--session", "s", "--query", "hi", "--limit", "1"]);
    assert_eq!(found.pointer("/hits/0/session"), Some(&json!("s")), "{found}");
    let set = ["fact", "set", "--session", "s", "--key", "s/k", "--content", "v"];
    assert_eq!(verb(&dir.0, &home, &set)["committed"], true);
    let got = verb(&dir.0, &home, &["fact", "get", "--session", "s", "--key", "s/k"]);
    assert_eq!(got.pointer("/fact/content"), Some(&json!("v")), "{got}");
    let recent = verb(&dir.0, &home, &["recent", "--session", "s", "--n", "1"]);
    assert_eq!(recent.pointer("/turns/0/content"), Some(&json!("hi x")), "{recent}");
    // Each verb after the first found the same server.
    assert_eq!(std::fs::read_to_string(home.join("data/LOCK")).unwrap(), lock);
}

/// `atticdb mcp` on a fresh home: initialize, tools/list and a memory_search call.
#[test]
fn mcp_round_trip_on_a_fresh_home() {
    let dir = Dir::new();
    let home = dir.0.join("home");
    let _servers = Servers(vec![home.join("data")]);
    let mut command = atticdb(&dir.0, &[("ATTICDB_HOME", home.as_path())]);
    command.args(["mcp", "--session", "s"]);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let err = drain(child.stderr.take().unwrap());
    let (sender, lines) = channel();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in stdout.lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    // Sends `message`, then reads 1 line when `reply`.
    let mut send = |message: Value, reply: bool| {
        writeln!(stdin, "{message}").unwrap();
        reply.then(|| {
            let line = lines.recv_timeout(Duration::from_secs(60)).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        })
    };
    let params = json!({"protocolVersion": "2025-11-25", "capabilities": {},
                        "clientInfo": {"name": "test", "version": "1"}});
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params});
    let init = send(init, true).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), false);
    let tools = send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), true).unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"memory_search"), "{tools}");
    let append = json!({"name": "memory_append", "arguments": {"content": "hello mcp"}});
    let append = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": append});
    let appended = send(append, true).unwrap();
    assert_ne!(appended["result"]["isError"], true, "{appended}");
    let search = json!({"name": "memory_search", "arguments": {"query": "hello"}});
    let search = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": search});
    let found = send(search, true).unwrap();
    assert_eq!(found["id"], 4, "{found}");
    assert_ne!(found["result"]["isError"], true, "{found}");
    let text = found["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("hello mcp"), "{found}");
    drop(send);
    drop(stdin);
    let status = wait(&mut child, COMMAND_TIMEOUT);
    let err = err.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    assert_eq!(status.and_then(|status| status.code()), Some(0), "{err}");
    assert!(err.contains("atticdb: started server pid "), "{err}");
}

/// `verify` copies a data directory under its lock. A live server holds the lock, so it
/// refuses with 75. Once the server is stopped, it passes.
#[test]
fn verify_respects_the_server_lock() {
    let dir = Dir::new();
    let home = dir.0.join("home");
    let data = home.join("data");
    let servers = Servers(vec![data.clone()]);
    let env = [("ATTICDB_HOME", home.as_path())];
    let (code, out, err) = run(&dir.0, &env, &["append", "--session", "s", "--content", "x"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let verify = ["verify", "--data-dir", data.to_str().unwrap()];
    let (code, out, err) = run(&dir.0, &[], &verify);
    assert_eq!(code, Some(75), "{out}{err}");
    assert!(err.contains("locked by pid="), "{err}");
    drop(servers);
    let (code, out, err) = run(&dir.0, &[], &verify);
    assert_eq!(code, Some(0), "{out}{err}");
}

/// Squatters on the port in `LOCK`, discovery against a real server with many tenants, and
/// the user profile check.
#[cfg(windows)]
mod windows {
    use super::{atticdb, locked, run, wait, Dir, Servers};
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    const TOKEN: &str = "token-a-0123456789abcdefghijklmnopqrstuv";

    /// What a squatter answers.
    enum Reply {
        /// This `/healthz` body.
        Body(String),
        /// Whatever the server at this address answers, passed on unchanged.
        Relay(String),
    }

    /// A listener that is not ours. It records every byte it receives, from every connection.
    struct Squatter {
        addr: String,
        seen: Arc<Mutex<Vec<u8>>>,
        relayed: Arc<Mutex<Vec<u8>>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    /// Reads a request or response head, and a body by its `Content-Length`.
    fn read_message(stream: &mut TcpStream) -> Vec<u8> {
        let mut message = Vec::new();
        let mut chunk = [0; 4096];
        let complete = |message: &[u8]| {
            let Some(end) = message.windows(4).position(|window| window == b"\r\n\r\n") else {
                return false;
            };
            let head = String::from_utf8_lossy(&message[..end]).to_ascii_lowercase();
            let length = head.lines().find_map(|line| line.strip_prefix("content-length: "));
            let length: usize = length.and_then(|n| n.trim().parse().ok()).unwrap_or(0);
            message.len() >= end + 4 + length
        };
        while !complete(&message) {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => message.extend_from_slice(&chunk[..n]),
            }
        }
        message
    }

    impl Squatter {
        fn start(reply: Reply) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let (seen, stop) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
            let relayed = Arc::new(Mutex::new(Vec::new()));
            let (log, stopping, passed) = (seen.clone(), stop.clone(), relayed.clone());
            let thread = std::thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                    let mut request = read_message(&mut stream);
                    let answer = match &reply {
                        Reply::Body(body) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .into_bytes(),
                        Reply::Relay(server) => {
                            let mut upstream = TcpStream::connect(server).unwrap();
                            upstream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                            upstream.write_all(&request).unwrap();
                            let answer = read_message(&mut upstream);
                            passed.lock().unwrap().extend_from_slice(&answer);
                            answer
                        }
                    };
                    let _ = stream.write_all(&answer);
                    // A token sent after the answer would arrive here.
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                    let mut chunk = [0; 4096];
                    while let Ok(n) = stream.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..n]);
                    }
                    log.lock().unwrap().extend_from_slice(&request);
                }
            });
            Self { addr, seen, relayed, stop, thread: Some(thread) }
        }

        /// A squatter that answers as instance 0123 with these `proofs`.
        fn answering(proofs: Value) -> Self {
            let body = json!({"status": "ok", "instance": "0123", "proofs": proofs});
            Self::start(Reply::Body(body.to_string()))
        }

        /// Stops it. Returns all it received, and all it relayed back.
        fn finish(mut self) -> (String, String) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
            let text = |bytes: &Mutex<Vec<u8>>| {
                String::from_utf8_lossy(&bytes.lock().unwrap()).into_owned()
            };
            (text(&self.seen), text(&self.relayed))
        }

        fn lock(&self, data: &Path, instance: &str) {
            std::fs::create_dir_all(data).unwrap();
            let text = format!("pid=1 instance={instance} tcp={}\n", self.addr);
            std::fs::write(data.join("LOCK"), text).unwrap();
        }
    }

    fn assert_never_got_the_token(seen: &str, token: &str) {
        assert!(seen.contains("GET /healthz HTTP/1.1"), "it was probed: {seen}");
        assert!(seen.to_ascii_lowercase().contains("x-atticdb-nonce: "), "{seen}");
        assert!(!seen.to_ascii_lowercase().contains("authorization"), "{seen}");
        assert!(!seen.contains(token), "{seen}");
    }

    /// `append` on `data` with the token in a file and a 3 s retry deadline. No config, so it
    /// starts no server.
    fn append_to(dir: &Dir, data: &Path, token: &str) -> (Option<i32>, String, String) {
        let file = dir.0.join("token");
        std::fs::write(&file, token).unwrap();
        let (data, file) = (data.to_str().unwrap(), file.to_str().unwrap());
        let args = ["append", "--data-dir", data, "--token-file", file];
        let args = [&args[..], &["--session", "s", "--retry-deadline", "3s", "--content", "x"]];
        run(&dir.0, &[], &args.concat())
    }

    /// With no config to start a server from, the command waits for a trusted server and
    /// fails. The squatter gets only nonces.
    #[test]
    fn a_squatter_with_a_wrong_proof_never_gets_the_token() {
        let dir = Dir::new();
        let data = dir.0.join("data");
        let squatter = Squatter::answering(json!(["00".repeat(32)]));
        squatter.lock(&data, "0123");
        let (code, out, err) = append_to(&dir, &data, TOKEN);
        let (seen, _) = squatter.finish();
        assert_ne!(code, Some(0), "{out}{err}");
        assert!(err.contains("gave no proof"), "{err}");
        assert_never_got_the_token(&seen, TOKEN);
    }

    /// In a home, the command starts its own server, which takes over `LOCK`, and succeeds
    /// there. The squatter, with no proof at all, still gets only nonces.
    #[test]
    fn a_home_command_skips_a_squatter_without_a_proof() {
        let dir = Dir::new();
        let home = dir.0.join("home");
        let data = home.join("data");
        let _servers = Servers(vec![data.clone()]);
        let squatter = Squatter::answering(json!([]));
        squatter.lock(&data, "0123");
        let env = [("ATTICDB_HOME", home.as_path())];
        let (code, out, err) = run(&dir.0, &env, &["append", "--session", "s", "--content", "x"]);
        let (seen, _) = squatter.finish();
        assert_eq!(code, Some(0), "{out}{err}");
        assert!(err.contains("gave no proof"), "{err}");
        assert!(locked(&data), "its own server holds the lock");
        let lock = std::fs::read_to_string(data.join("LOCK")).unwrap();
        assert!(!lock.contains("instance=0123 "), "{lock}");
        assert_never_got_the_token(&seen, &home_token(&home));
    }

    fn home_token(home: &Path) -> String {
        let config = std::fs::read_to_string(home.join("atticdb.json")).unwrap();
        let config: Value = serde_json::from_str(&config).unwrap();
        config["tenants"][0]["token"].as_str().unwrap().to_owned()
    }

    /// The real server's `instance` and `tcp` from its `LOCK`.
    fn lock_fields(data: &Path) -> (String, String) {
        let lock = std::fs::read_to_string(data.join("LOCK")).unwrap();
        let field = |name: &str| {
            let start = lock.find(name).unwrap() + name.len();
            lock[start..].split_whitespace().next().unwrap().to_owned()
        };
        (field("instance="), field("tcp="))
    }

    /// A squatter names the live server's instance and relays the nonce to it. The real
    /// proofs come back unchanged. They are bound to the server's own address, so they fail.
    #[test]
    fn a_relay_to_the_real_server_never_gets_the_token() {
        let dir = Dir::new();
        let home = dir.0.join("home");
        let _servers = Servers(vec![home.join("data")]);
        let env = [("ATTICDB_HOME", home.as_path())];
        let (code, out, err) = run(&dir.0, &env, &["append", "--session", "s", "--content", "x"]);
        assert_eq!(code, Some(0), "{out}{err}");
        let (instance, server) = lock_fields(&home.join("data"));
        let token = home_token(&home);
        let squatter = Squatter::start(Reply::Relay(server));
        let data = dir.0.join("other");
        squatter.lock(&data, &instance);
        let (code, out, err) = append_to(&dir, &data, &token);
        let (seen, relayed) = squatter.finish();
        assert_ne!(code, Some(0), "{out}{err}");
        assert!(err.contains("gave no proof"), "{err}");
        assert!(relayed.contains("\"proofs\":[\""), "the real server answered: {relayed}");
        assert_never_got_the_token(&seen, &token);
    }

    /// A real server with 100 tenants answers with over 6 KiB of proofs. A client with the
    /// last tenant's token finds it and writes.
    #[test]
    fn discovery_finds_a_server_with_100_tenants() {
        let dir = Dir::new();
        let data = dir.0.join("data");
        let _servers = Servers(vec![data.clone()]);
        let token = |n: usize| format!("token-{n:03}-0123456789abcdefghijklmnopqrstuv");
        let tenants: Vec<Value> =
            (0..100).map(|n| json!({"name": format!("t{n}"), "token": token(n)})).collect();
        let config = dir.0.join("server.json");
        std::fs::write(&config, json!({"tenants": tenants}).to_string()).unwrap();
        let mut serve = atticdb(&dir.0, &[]);
        serve.args(["serve", "--config", config.to_str().unwrap(), "--data-dir"]);
        serve.args([data.to_str().unwrap(), "--listen", "127.0.0.1:0"]);
        serve.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = serve.spawn().unwrap();
        // Its engine is open once it prints the serving line.
        let (sender, lines) = std::sync::mpsc::channel();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        std::thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line =
                lines.recv_timeout(left).unwrap_or_else(|_| panic!("the server did not start"));
            if line.starts_with("atticdb: serving ") {
                break;
            }
        }
        let (code, out, err) = append_to(&dir, &data, &token(99));
        assert_eq!(code, Some(0), "{out}{err}");
        let _ = child.kill();
        let _ = wait(&mut child, Duration::from_secs(10));
    }

    /// Every path that picks the store must be inside the user profile.
    #[test]
    fn paths_outside_the_profile_are_refused() {
        let dir = Dir::new();
        let profile = dir.0.join("profile");
        let local = profile.join("AppData").join("Local");
        std::fs::create_dir_all(&local).unwrap();
        let outside = dir.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let config = outside.join("atticdb.json");
        let tenants = json!([{"name": "me", "token": TOKEN}]);
        std::fs::write(&config, json!({"tenants": tenants}).to_string()).unwrap();
        let inside = local.join("atticdb.json");
        let data = outside.join("data");
        let with_data = json!({"data_dir": data, "tenants": tenants});
        std::fs::write(&inside, with_data.to_string()).unwrap();
        let token = profile.join("token");
        std::fs::write(&token, TOKEN).unwrap();
        let profile_env = [("USERPROFILE", profile.as_path()), ("LOCALAPPDATA", local.as_path())];
        let (config, inside, data, token) = (
            config.to_str().unwrap(),
            inside.to_str().unwrap(),
            data.to_str().unwrap(),
            token.to_str().unwrap(),
        );
        let home = outside.join("home");
        let cases: [(&[&str], Option<&Path>, &str); 4] = [
            (&[], Some(home.as_path()), "home"),
            (&["--data-dir", data, "--token-file", token], None, "data dir"),
            (&["--config", config, "--tenant", "me"], None, "config"),
            (&["--config", inside, "--tenant", "me"], None, "data dir"),
        ];
        for (flags, home, what) in cases {
            let mut env = profile_env.to_vec();
            env.extend(home.map(|home| ("ATTICDB_HOME", home)));
            let args =
                [&["append", "--content", "x", "--retry-deadline", "2s"][..], flags].concat();
            let (code, out, err) = run(&dir.0, &env, &args);
            assert_ne!(code, Some(0), "{args:?}: {out}{err}");
            assert!(err.contains(&format!("{what} ")) && err.contains("(D92)"), "{args:?}: {err}");
            assert!(!locked(&outside.join("data")), "{args:?}: nothing started");
        }
        assert!(!home.exists(), "no home was made outside the profile");
        // The default home, inside the profile, works.
        let _servers = Servers(vec![local.join("atticdb").join("data")]);
        let (code, out, err) = run(&dir.0, &profile_env, &["append", "--content", "x"]);
        assert_eq!(code, Some(0), "{out}{err}");
    }

    /// The server and every token file obey the same rule. Config and token reads check the
    /// path. `serve` checks its data dir.
    #[test]
    fn server_and_token_paths_outside_the_profile_are_refused() {
        let dir = Dir::new();
        let profile = dir.0.join("profile");
        let local = profile.join("AppData").join("Local");
        let outside = dir.0.join("outside");
        for folder in [&local, &outside] {
            std::fs::create_dir_all(folder).unwrap();
        }
        let tenants = json!([{"name": "me", "token": TOKEN}]);
        let write = |path: &Path, text: String| {
            std::fs::write(path, text).unwrap();
            path.to_str().unwrap().to_owned()
        };
        let config_out =
            write(&outside.join("server.json"), json!({"tenants": tenants}).to_string());
        let config_in =
            write(&profile.join("server.json"), json!({"tenants": tenants}).to_string());
        let outside_data = outside.join("data");
        let with_data = json!({"data_dir": outside_data, "tenants": tenants}).to_string();
        let config_data_out = write(&profile.join("data-out.json"), with_data);
        let token_out = write(&outside.join("token"), TOKEN.to_owned());
        let data_in = profile.join("data");
        let _servers = Servers(vec![data_in.clone(), outside_data.clone()]);
        let (data_in, data_out) = (data_in.to_str().unwrap(), outside_data.to_str().unwrap());
        fn serve<'a>(config: &'a str, data: Option<&'a str>) -> Vec<&'a str> {
            let mut args = vec!["serve", "--config", config, "--listen", "127.0.0.1:0"];
            args.extend(data.map(|data| ["--data-dir", data]).into_iter().flatten());
            args
        }
        let append = ["append", "--data-dir", data_in, "--token-file", &token_out];
        let append = [&append[..], &["--content", "x", "--retry-deadline", "2s"]].concat();
        let load = ["load", "--url", "http://127.0.0.1:1", "--token-file", &token_out];
        let cases: [(Vec<&str>, &str); 5] = [
            (serve(&config_out, Some(data_in)), "config "),
            (serve(&config_in, Some(data_out)), "data dir "),
            (serve(&config_data_out, None), "data dir "),
            (append, "token file "),
            (load.to_vec(), "token file "),
        ];
        let env = [("USERPROFILE", profile.as_path()), ("LOCALAPPDATA", local.as_path())];
        for (args, what) in cases {
            let (code, out, err) = run(&dir.0, &env, &args);
            assert_ne!(code, Some(0), "{args:?}: {out}{err}");
            assert!(err.contains(what) && err.contains("(D92)"), "{args:?}: {err}");
            assert!(!outside_data.exists(), "{args:?}: no data dir was made outside");
        }
    }

    /// Nothing private is made outside the profile: no token file from `init`, and no copy
    /// of the store from `verify --scratch`.
    #[test]
    fn private_files_are_never_made_outside_the_profile() {
        let dir = Dir::new();
        let profile = dir.0.join("profile");
        let local = profile.join("AppData").join("Local");
        let outside = dir.0.join("outside");
        for folder in [&local, &outside] {
            std::fs::create_dir_all(folder).unwrap();
        }
        let env = [("USERPROFILE", profile.as_path()), ("LOCALAPPDATA", local.as_path())];
        let empty = |what: &str| {
            let left: Vec<_> = std::fs::read_dir(&outside).unwrap().collect();
            assert!(left.is_empty(), "{what}: made {left:?}");
        };
        let config = outside.join("atticdb.json");
        let init = ["init", "--config", config.to_str().unwrap(), "--tenant", "me"];
        let (code, out, err) = run(&dir.0, &env, &init);
        assert_ne!(code, Some(0), "{out}{err}");
        assert!(err.contains("config ") && err.contains("(D92)"), "{err}");
        assert!(!out.contains("token"), "{out}");
        empty("init");

        let home = profile.join("home");
        let data = home.join("data");
        let servers = Servers(vec![data.clone()]);
        let mut with_home = env.to_vec();
        with_home.push(("ATTICDB_HOME", home.as_path()));
        let (code, out, err) = run(&dir.0, &with_home, &["append", "--content", "x"]);
        assert_eq!(code, Some(0), "{out}{err}");
        drop(servers);
        let data = data.to_str().unwrap();
        let verify = ["verify", "--data-dir", data, "--scratch", outside.to_str().unwrap()];
        let (code, out, err) = run(&dir.0, &env, &verify);
        assert_ne!(code, Some(0), "{out}{err}");
        assert!(err.contains("scratch copy ") && err.contains("(D92)"), "{err}");
        empty("verify");
        let scratch = profile.join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let verify = ["verify", "--data-dir", data, "--scratch", scratch.to_str().unwrap()];
        let (code, out, err) = run(&dir.0, &env, &verify);
        assert_eq!(code, Some(0), "a scratch dir in the profile works: {out}{err}");
    }
}
