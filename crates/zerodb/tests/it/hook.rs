use crate::support::{chmod, Dir, Holders, Process};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs `zerodb` with `HOME` set to `home` and writes `stdin`, keeping it open until the
/// exit. Returns the exit code, stdout and stderr.
fn zerodb(home: &Path, args: &[&str], stdin: &[u8]) -> (Option<i32>, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zerodb"));
    command.args(args).current_dir(home).env("HOME", home).stdin(Stdio::piped());
    for var in ["COPILOT_HOME", "XDG_DATA_HOME", "ZERODB_TOKEN"] {
        command.env_remove(var);
    }
    let mut process = Process::start(&mut command);
    let mut input = process.child.stdin.take().unwrap();
    // The hook reads at most 1 MiB, so a longer write fails once it exits.
    let _ = input.write_all(stdin);
    let (status, _) = process.wait();
    (status.code(), process.stdout(), process.stderr())
}

fn hook(home: &Path, stdin: &str) -> String {
    let (code, out, err) = zerodb(home, &["hook", "session-start"], stdin.as_bytes());
    assert_eq!(code, Some(0), "{stdin:.80}: {err}");
    assert_eq!(out.lines().count(), 1, "{stdin:.80}: 1 line: {out}");
    out
}

fn seed(home: &Path, args: &[&str]) {
    let (code, _, err) = zerodb(home, args, b"");
    assert_eq!(code, Some(0), "{args:?}: {err}");
}

fn cwd(path: &Path) -> String {
    json!({"cwd": path}).to_string()
}

fn data_dir(home: &Path) -> std::path::PathBuf {
    let data =
        if cfg!(target_os = "macos") { "Library/Application Support" } else { ".local/share" };
    home.join(data).join("zerodb/data")
}

/// A trusted fake server in `home`'s data directory. `/healthz` answers at once, any other
/// request after 1.5 s with no facts and no turns.
fn slow_server(home: &Path) {
    let data = data_dir(home);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&data).unwrap();
    let socket = data.join("zerodb.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let lock =
        format!("pid={} instance=X unix={} tcp=none\n", std::process::id(), socket.display());
    std::fs::write(data.join("LOCK"), lock).unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut head = String::new();
                    while !head.ends_with("\r\n\r\n") {
                        if reader.read_line(&mut head).unwrap_or(0) == 0 {
                            return;
                        }
                    }
                    let body = if head.starts_with("GET /healthz ") {
                        r#"{"instance": "X"}"#
                    } else {
                        std::thread::sleep(Duration::from_millis(1_500));
                        r#"{"facts": [], "next": null, "turns": [], "truncated": false}"#
                    };
                    let length = body.len();
                    let reply =
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n{body}");
                    if stream.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
}

/// The block lists rules, then this repo's facts, both in key order, then the newest 5
/// turns of the repo's session from both Copilot agents, oldest first. Stdin stays open.
#[test]
fn session_start_prints_the_memory_block() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir_all(home.join("app/.git")).unwrap();
    std::fs::create_dir_all(home.join("app/src")).unwrap();
    std::fs::create_dir_all(home.join("big")).unwrap();
    std::fs::write(home.join("big/.git"), "gitdir: elsewhere\n").unwrap();
    let _holders = Holders(vec![data_dir(&home)]);
    let long = "é".repeat(301);
    let facts = [
        ("user/tabs", "line 1\nline 2"),
        ("repo/app/long", &long),
        ("user/style", "short sentences"),
        ("repo/app/build", "cargo build"),
        ("repo/other/x", "not shown"),
    ];
    for (key, content) in facts {
        seed(&home, &["fact", "set", "--key", key, "--content", content]);
    }
    let turns = [
        ("vscode-copilot", "app", "v1"),
        ("copilot", "app", "c1"),
        ("vscode-copilot", "app", "v2"),
        ("copilot", "other", "not shown"),
        ("mcp", "app", "not shown"),
        ("copilot", "app", "c2"),
        ("vscode-copilot", "app", "v3"),
        ("copilot", "app", "c3"),
    ];
    for (agent, session, content) in turns {
        seed(&home, &["append", "--agent", agent, "--session", session, "--content", content]);
    }
    let out: Value = serde_json::from_str(&hook(&home, &cwd(&home.join("app/src")))).unwrap();
    let block = format!(
        "zerodb memory (snapshot at chat start; call memory_search or fact_list for more)\n\
         Rules:\n- user/style: short sentences\n- user/tabs: line 1 line 2\n\
         This repo (app):\n- repo/app/build: cargo build\n- repo/app/long: {}\n\
         Recent notes (app):\n- c1\n- v2\n- c2\n- v3\n- c3",
        "é".repeat(300)
    );
    assert_eq!(out, json!({"additionalContext": block}));

    // 100 facts of 300 characters pass the cap, which stops at a whole line.
    let content = "x".repeat(300);
    for n in 0..100 {
        seed(&home, &["fact", "set", "--key", &format!("repo/big/{n:03}"), "--content", &content]);
    }
    let out: Value = serde_json::from_str(&hook(&home, &cwd(&home.join("big")))).unwrap();
    let block = out["additionalContext"].as_str().unwrap();
    let length = block.chars().count();
    assert!((6_000 - 320..=6_000).contains(&length), "{length} characters");
    let (body, end) = block.rsplit_once('\n').unwrap();
    assert_eq!(end, "(more in zerodb)");
    let last = body.lines().last().unwrap();
    assert!(last.starts_with("- repo/big/") && last.ends_with(&format!(": {content}")), "{last}");
}

/// Each row prints `{}` and exits 0 in under 5 s. A slow server gets 3 s for all 4 reads.
#[test]
fn session_start_prints_nothing_on_errors() {
    let dir = Dir::new();
    let (empty, refused, slow) = (dir.0.join("e"), dir.0.join("r"), dir.0.join("s"));
    let _holders = Holders(vec![data_dir(&empty), data_dir(&slow)]);
    slow_server(&slow);
    std::fs::create_dir_all(data_dir(&refused)).unwrap();
    chmod(data_dir(&refused).parent().unwrap(), 0o700);
    chmod(&data_dir(&refused), 0o755);
    let too_long = format!(r#"{{"cwd": "{}"}}"#, "a".repeat(2 << 20));
    let rows = [
        ("not JSON", &empty, "nope".to_owned()),
        ("no cwd", &empty, r#"{"dir": "/tmp"}"#.to_owned()),
        ("over 1 MiB", &empty, too_long),
        ("no memory", &empty, cwd(&empty.join("repo"))),
        ("data directory mode 0755", &refused, cwd(&refused.join("repo"))),
        ("1.5 s per read", &slow, cwd(&slow.join("repo"))),
    ];
    for (case, home, stdin) in rows {
        std::fs::create_dir_all(home).unwrap();
        let started = Instant::now();
        assert_eq!(hook(home, &stdin), "{}\n", "{case}");
        assert!(started.elapsed() < Duration::from_secs(5), "{case}: {:?}", started.elapsed());
    }
}
