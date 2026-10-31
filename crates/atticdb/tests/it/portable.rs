//! The home flow on every platform: a fresh home starts a server, the shell verbs and the MCP
//! adapter use it. On Windows that server is on loopback TCP, found through `LOCK` with a
//! token proof. The squatter test is Windows only, as Unix discovery never uses `tcp=`.
//!
//! Self-contained, as `support` is Unix only. Every server is stopped by the pid in `LOCK`.

use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// Variables that could lead a test to a real home. No spawn inherits them.
const HOME_VARS: [&str; 10] = [
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
const COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

// A private, short directory, removed on drop. Unix uses `support::Dir`, whose short path
// fits a socket.
#[cfg(unix)]
use crate::support::Dir;

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
fn locked(data: &Path) -> bool {
    File::open(data.join("LOCK")).is_ok_and(|file| file.try_lock().is_err())
}

/// The pid in `data`'s `LOCK`.
#[cfg(windows)]
fn lock_pid(data: &Path) -> Option<u32> {
    let lock = std::fs::read_to_string(data.join("LOCK")).ok()?;
    lock.strip_prefix("pid=")?.split(' ').next()?.parse().ok()
}

/// Stops the servers of these data directories at drop, also when a test fails.
struct Servers(Vec<PathBuf>);

impl Drop for Servers {
    fn drop(&mut self) {
        #[cfg(unix)]
        crate::support::stop_servers(&self.0);
        #[cfg(windows)]
        stop_servers(&self.0);
    }
}

/// Kills the server named in each `LOCK`, by pid and only when that pid runs `atticdb.exe`,
/// until no lock was held for 500 ms, for up to 15 s.
#[cfg(windows)]
fn stop_servers(dirs: &[PathBuf]) {
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
fn drain(mut from: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = from.read_to_end(&mut text);
        let _ = sender.send(String::from_utf8_lossy(&text).into_owned());
    });
    receiver
}

/// Waits up to `limit` for `child`, else kills it and returns `None`.
fn wait(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
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

/// `atticdb` without [`HOME_VARS`], in `cwd`, with `env` added.
fn atticdb(cwd: &Path, env: &[(&str, &Path)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_atticdb"));
    for var in HOME_VARS {
        command.env_remove(var);
    }
    command.current_dir(cwd);
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

/// A listener that is not ours sits on the port in `LOCK` and answers `/healthz` with the
/// `LOCK` instance and a wrong or missing proof. No command sends it the token.
#[cfg(windows)]
mod squatter {
    use super::{locked, run, Dir, Servers};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    const TOKEN: &str = "token-a-0123456789abcdefghijklmnopqrstuv";

    /// Records every byte it receives, from every connection.
    struct Squatter {
        addr: String,
        seen: Arc<Mutex<Vec<u8>>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Squatter {
        /// `proof` is the `proofs` array it answers with.
        fn start(proof: serde_json::Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let (seen, stop) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
            let (log, stopping) = (seen.clone(), stop.clone());
            let body = serde_json::json!({"status": "ok", "instance": "0123", "proofs": proof});
            let body = body.to_string();
            let thread = std::thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                    let mut chunk = [0; 4096];
                    let mut head = Vec::new();
                    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => head.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(answer.as_bytes());
                    // A token sent after the answer would arrive here.
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                    while let Ok(n) = stream.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        head.extend_from_slice(&chunk[..n]);
                    }
                    log.lock().unwrap().extend_from_slice(&head);
                }
            });
            Self { addr, seen, stop, thread: Some(thread) }
        }

        /// Stops it and returns all it received.
        fn finish(mut self) -> String {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
            String::from_utf8_lossy(&self.seen.lock().unwrap()).into_owned()
        }

        fn lock(&self, data: &Path) {
            std::fs::create_dir_all(data).unwrap();
            let text = format!("pid=1 instance=0123 tcp={}\n", self.addr);
            std::fs::write(data.join("LOCK"), text).unwrap();
        }
    }

    fn assert_never_got_the_token(seen: &str, token: &str) {
        assert!(seen.contains("GET /healthz HTTP/1.1"), "it was probed: {seen}");
        assert!(seen.to_ascii_lowercase().contains("x-atticdb-nonce: "), "{seen}");
        assert!(!seen.to_ascii_lowercase().contains("authorization"), "{seen}");
        assert!(!seen.contains(token), "{seen}");
    }

    /// With no config to start a server from, the command waits for a trusted server and
    /// fails. The squatter gets only nonces.
    #[test]
    fn a_squatter_with_a_wrong_proof_never_gets_the_token() {
        let dir = Dir::new();
        let data = dir.0.join("data");
        let squatter = Squatter::start(serde_json::json!(["00".repeat(32)]));
        squatter.lock(&data);
        let token = dir.0.join("token");
        std::fs::write(&token, TOKEN).unwrap();
        let args = [
            "append",
            "--data-dir",
            data.to_str().unwrap(),
            "--token-file",
            token.to_str().unwrap(),
            "--session",
            "s",
            "--retry-deadline",
            "3s",
            "--content",
            "x",
        ];
        let (code, out, err) = run(&dir.0, &[], &args);
        let seen = squatter.finish();
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
        let squatter = Squatter::start(serde_json::json!([]));
        squatter.lock(&data);
        let env = [("ATTICDB_HOME", home.as_path())];
        let (code, out, err) = run(&dir.0, &env, &["append", "--session", "s", "--content", "x"]);
        let seen = squatter.finish();
        assert_eq!(code, Some(0), "{out}{err}");
        assert!(err.contains("gave no proof"), "{err}");
        assert!(locked(&data), "its own server holds the lock");
        let lock = std::fs::read_to_string(data.join("LOCK")).unwrap();
        assert!(!lock.contains("instance=0123 "), "{lock}");
        let config = std::fs::read_to_string(home.join("atticdb.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        let token = config["tenants"][0]["token"].as_str().unwrap();
        assert_never_got_the_token(&seen, token);
    }
}
