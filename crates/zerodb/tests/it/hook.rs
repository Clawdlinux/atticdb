use crate::support::{chmod, Dir, Holders, Process};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type Output = (Option<i32>, String, String);

/// Runs `zerodb` in `cwd` with `HOME` set to `home` and writes `stdin` after `delay`, keeping
/// it open until the exit. A stray `ZERODB_TOKEN` is set, which the home's users ignore.
/// Returns the exit code, stdout and stderr.
fn zerodb(home: &Path, cwd: &Path, args: &[&str], stdin: &[u8], delay: u64) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zerodb"));
    command.args(args).current_dir(cwd).env("HOME", home).stdin(Stdio::piped());
    for var in ["COPILOT_HOME", "XDG_DATA_HOME"] {
        command.env_remove(var);
    }
    command.env("ZERODB_TOKEN", "x");
    let mut process = Process::start(&mut command);
    let mut input = process.child.stdin.take().unwrap();
    std::thread::sleep(Duration::from_millis(delay));
    // The hook reads at most 1 MiB, so a longer write fails once it exits.
    let _ = input.write_all(stdin);
    let (status, _) = process.wait();
    (status.code(), process.stdout(), process.stderr())
}

/// The hook's stdout and stderr, after it exits 0 with 1 line.
fn hook(home: &Path, stdin: &str, delay: u64) -> (String, String) {
    let (code, out, err) = zerodb(home, home, &["hook", "session-start"], stdin.as_bytes(), delay);
    assert_eq!(code, Some(0), "{stdin:.80}: {err}");
    assert_eq!(out.lines().count(), 1, "{stdin:.80}: 1 line: {out}");
    (out, err)
}

fn seed(home: &Path, args: &[&str]) {
    let (code, _, err) = zerodb(home, home, args, b"", 0);
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
    // 2 rules over half of `fact_list`'s 64 KiB page, and a newest note over 64 KiB.
    let (rule_1, rule_2, note) = ("1".repeat(40 << 10), "2".repeat(40 << 10), "n".repeat(70 << 10));
    let facts = [
        ("user/big-2", rule_2.as_str()),
        ("user/big-1", &rule_1),
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
        ("copilot", "", "c3"),
        ("copilot", "app", &note),
    ];
    for (agent, session, content) in turns {
        let mut args = vec!["append", "--agent", agent, "--content", content];
        // Without `--session`, the session is the repo of the current directory, `app`.
        let cwd = if session.is_empty() { home.join("app/src") } else { home.clone() };
        args.extend(["--session", session].iter().filter(|_| !session.is_empty()));
        let (code, _, err) = zerodb(&home, &cwd, &args, b"", 0);
        assert_eq!(code, Some(0), "{args:?}: {err}");
    }
    // A symlinked cwd names the repo it points into, as the adapter's `current_dir` does.
    std::os::unix::fs::symlink(home.join("app/src"), home.join("link")).unwrap();
    let out: Value = serde_json::from_str(&hook(&home, &cwd(&home.join("link")), 0).0).unwrap();
    let block = format!(
        "zerodb memory for repo app (facts under repo/app/; snapshot at chat start; call \
         memory_search or fact_list for more)\n\
         Rules:\n- user/big-1: {}\n- user/big-2: {}\n\
         - user/style: short sentences\n- user/tabs: line 1 line 2\n\
         This repo (app):\n- repo/app/build: cargo build\n- repo/app/long: {}\n\
         Recent notes (app):\n- v2\n- c2\n- v3\n- c3\n- {}",
        &rule_1[..300],
        &rule_2[..300],
        "é".repeat(300),
        &note[..300]
    );
    assert_eq!(out, json!({"additionalContext": block}));

    // 100 facts of 300 characters pass the cap, which stops at a whole line.
    let content = "x".repeat(300);
    for n in 0..100 {
        seed(&home, &["fact", "set", "--key", &format!("repo/big/{n:03}"), "--content", &content]);
    }
    let out: Value = serde_json::from_str(&hook(&home, &cwd(&home.join("big")), 0).0).unwrap();
    let block = out["additionalContext"].as_str().unwrap();
    let length = block.chars().count();
    assert!((6_000 - 320..=6_000).contains(&length), "{length} characters");
    let (body, end) = block.rsplit_once('\n').unwrap();
    assert_eq!(end, "(more in zerodb)");
    let last = body.lines().last().unwrap();
    assert!(last.starts_with("- repo/big/") && last.ends_with(&format!(": {content}")), "{last}");
}

/// Each row prints `{}` and exits 0 within its time range, saying why on stderr. A slow
/// server gets 3 s from the start for all 4 reads. Stdin open after part of a value hits
/// the 4 s stop.
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
    let (late, stop) = ("less than 2 s left before the deadline", "stopped at 4 s");
    let rows = [
        ("not JSON", &empty, "nope".to_owned(), 0, 0..5_000, ""),
        ("no cwd", &empty, r#"{"dir": "/tmp"}"#.to_owned(), 0, 0..5_000, ""),
        ("over 1 MiB", &empty, too_long, 0, 0..5_000, ""),
        ("no memory", &empty, cwd(&empty.join("repo")), 0, 0..5_000, ""),
        ("data directory mode 0755", &refused, cwd(&refused.join("repo")), 0, 0..5_000, ""),
        ("1.5 s per read", &slow, cwd(&slow.join("repo")), 0, 1_400..5_000, late),
        // The budget counts from the start, so the first read is already late.
        ("stdin after 1.2 s", &slow, cwd(&slow.join("repo")), 1_200, 1_200..2_000, late),
        ("partial JSON, stdin open", &empty, r#"{"cwd": "/tm"#.to_owned(), 0, 3_900..5_000, stop),
    ];
    for (case, home, stdin, delay, range, says) in rows {
        std::fs::create_dir_all(home).unwrap();
        let started = Instant::now();
        let (out, err) = hook(home, &stdin, delay);
        let elapsed = started.elapsed();
        assert_eq!(out, "{}\n", "{case}");
        assert!(range.contains(&(elapsed.as_millis() as u64)), "{case}: {elapsed:?}");
        assert!(err.contains(says) && err.contains(stop) == (says == stop), "{case}: {err}");
    }
}
