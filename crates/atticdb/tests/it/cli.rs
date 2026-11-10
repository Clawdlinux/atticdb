use crate::portable::{Dir, HOME_VARS};
#[cfg(unix)]
use crate::support::{atticdb, data_dir, Holders};
use std::process::{Command, Stdio};

/// The `atticdb` binary, without [`HOME_VARS`].
fn bare() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_atticdb"));
    for var in HOME_VARS {
        command.env_remove(var);
    }
    command
}

/// `--version` and `-V` print the name and the version on stdout and exit 0. They take no
/// other args. The home is never read, so a relative `ATTICDB_HOME` does not matter. A source
/// build prints the crate version plus `+dev`. A release build prints `ATTICDB_RELEASE_VERSION`.
#[test]
fn version_flag() {
    let dir = Dir::new();
    let want = match option_env!("ATTICDB_RELEASE_VERSION").filter(|v| !v.is_empty()) {
        Some(release) => format!("atticdb {release}\n"),
        None => format!("atticdb {}+dev\n", env!("CARGO_PKG_VERSION")),
    };
    assert_eq!(format!("atticdb {}\n", atticdb::VERSION), want);
    for flag in ["--version", "-V"] {
        let mut command = bare();
        command.arg(flag).current_dir(&dir.0).env("HOME", dir.0.join("h"));
        let out = command.env("ATTICDB_HOME", "rel").stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{flag}");
        assert!(out.stderr.is_empty(), "{flag}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!dir.0.join("h").exists() && !dir.0.join("rel").exists(), "{flag}: no home");

        let mut command = bare();
        command.args([flag, "extra"]).current_dir(&dir.0).env("HOME", dir.0.join("h"));
        let out = command.stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag} extra");
        assert!(String::from_utf8_lossy(&out.stderr).contains("atticdb --version | -V"));
    }
}

/// Runs `atticdb args` with no terminal, a fresh `HOME` and no home. Returns the exit code,
/// stdout and stderr. It also checks that no home was created.
fn run_bare(args: &[&str]) -> (Option<i32>, String, String) {
    let dir = Dir::new();
    let mut command = bare();
    command.args(args).current_dir(&dir.0).env("HOME", dir.0.join("h"));
    let out = command.stdin(Stdio::null()).output().unwrap();
    assert!(!dir.0.join("h").exists(), "{args:?}: no home");
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    (out.status.code(), text(&out.stdout), text(&out.stderr))
}

/// No args, `--help`, `-h` and `help` print the help screen on stdout and exit 0, whatever
/// follows them. With no terminal it has no color.
#[test]
fn help_screen() {
    let cases = [
        &[][..],
        &["--help"],
        &["-h"],
        &["help"],
        &["help", "setup"],
        &["help", "bogus"],
        &["--help", "extra"],
        &["-h", "x", "y"],
    ];
    for args in cases {
        let (code, out, err) = run_bare(args);
        assert_eq!(code, Some(0), "{args:?}: {err}");
        assert_eq!(out, atticdb::cli::help(atticdb::cli::Style::Plain, atticdb::VERSION));
        assert!(out.contains("Get started") && !out.contains('\x1b'), "{args:?}: {out}");
        assert!(err.is_empty(), "{args:?}: {err}");
    }
}

/// `help --all` prints every command and flag on stdout and exits 0.
#[test]
fn help_all() {
    let (code, out, err) = run_bare(&["help", "--all"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, format!("{}\n", atticdb::cli::USAGE));
    assert!(err.is_empty(), "{err}");
}

/// `--help` or `-h` right after a command, or after its verb, prints its usage lines on
/// stdout and exits 0.
#[test]
fn command_help() {
    for (args, command) in [
        (&["search", "--help"][..], "search"),
        (&["fact", "set", "--help"], "fact"),
        (&["connect", "--help"], "connect"),
        (&["fact", "get", "-h"], "fact"),
        (&["--version", "--help"], "--version"),
    ] {
        let (code, out, err) = run_bare(args);
        assert_eq!(code, Some(0), "{args:?}: {err}");
        assert_eq!(Some(out.as_str()), atticdb::cli::command_usage(command).as_deref());
        assert!(out.starts_with(&format!("usage: atticdb {command}")), "{args:?}: {out}");
        assert!(err.is_empty(), "{args:?}: {err}");
    }
    let (_, out, _) = run_bare(&["connect", "--help"]);
    assert!(out.ends_with("  claude-desktop\n") && out.contains("apps:  vscode"), "{out}");
    for (args, start) in [
        (&["setup", "-h"][..], "usage: atticdb setup"),
        (&["doctor", "-h"], "usage: atticdb doctor"),
    ] {
        let (code, out, err) = run_bare(args);
        assert_eq!(code, Some(0), "{args:?}: {err}");
        assert!(out.starts_with(start) && out.contains("\n\n"), "{args:?}: {out}");
    }
}

/// `-h` or `--help` anywhere but first is a normal argument, so a flag value is written as is.
#[cfg(unix)]
#[test]
fn help_tokens_as_values_are_written() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir_all(&home).unwrap();
    let _holders = Holders(vec![data_dir(&home)]);
    let run = |args: &[&str]| {
        let (code, out, err) = atticdb(&home, &home, args, &[], b"", 0);
        assert_eq!(code, Some(0), "{args:?}: {err}");
        assert!(!out.starts_with("usage:"), "{args:?}: {out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap_or_else(|e| panic!("{out}: {e}"))
    };
    let printed = run(&["append", "--content", "-h"]);
    assert_eq!(printed["committed"], true, "{printed}");
    let printed = run(&["recent", "--n", "1"]);
    assert_eq!(printed.pointer("/turns/0/content"), Some(&"-h".into()), "{printed}");
    let printed = run(&["fact", "set", "--key", "k", "--content", "--help"]);
    assert_eq!(printed["committed"], true, "{printed}");
    let printed = run(&["fact", "get", "--key", "k"]);
    assert_eq!(printed.pointer("/fact/content"), Some(&"--help".into()), "{printed}");
}

/// A usage error prints a short error on stderr and exits 2. Stdout stays empty.
#[test]
fn usage_errors() {
    let (code, out, err) = run_bare(&["connect", "claude-desktp"]);
    assert_eq!(code, Some(2), "{err}");
    assert_eq!(err.lines().next(), Some("error: unknown app \"claude-desktp\""), "{err}");
    assert!(err.contains("claude-desktop") && !err.contains('\x1b'), "{err}");
    assert!(out.is_empty(), "{out}");

    for (args, first) in [
        (&["bogus"][..], "error: unknown command \"bogus\""),
        (&["bogus", "--help"], "error: unknown command \"bogus\""),
    ] {
        let (code, out, err) = run_bare(args);
        assert_eq!(code, Some(2), "{args:?}: {err}");
        assert_eq!(err.lines().next(), Some(first), "{args:?}: {err}");
        assert!(err.ends_with("  more:  atticdb --help\n"), "{args:?}: {err}");
        assert!(out.is_empty(), "{args:?}: {out}");
    }
}

/// A first `append` on a fresh home starts a server and succeeds. Waiting for it is the
/// normal path, so stderr has no "no trusted server" line. `portable` covers Windows.
#[cfg(unix)]
#[test]
fn first_run_is_quiet() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir_all(&home).unwrap();
    let _holders = Holders(vec![data_dir(&home)]);
    let (code, out, err) = atticdb(&home, &home, &["append", "--content", "hi"], &[], b"", 0);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("\"committed\":true"), "{out}");
    assert!(err.contains("atticdb: started server pid "), "{err}");
    assert!(!err.contains("no trusted server"), "{err}");
}

/// `verify` and `dump` open the tape with the default options. A store made with another
/// segment size still opens, since the stored size wins.
#[test]
fn verify_and_dump_open_any_stored_segment_size() {
    use atticdb_core::{Engine, EngineOptions, NewTurn, Role, Scope};
    let dir = Dir::new();
    let data = dir.0.join("data");
    let tape = atticdb_tape::TapeOptions { segment_size: 1 << 20, ..Default::default() };
    let engine = Engine::open(&data, EngineOptions { tape, ..EngineOptions::default() }).unwrap();
    let scope = Scope { tenant: "a".into(), agent: "a".into(), session: "s".into() };
    let turn = NewTurn { role: Role::User, content: b"hi".to_vec(), meta: b"{}".to_vec() };
    engine.append(&scope, vec![turn], None).unwrap();
    drop(engine);
    let data = data.to_str().unwrap();
    for (verb, line) in [("verify", "health=ok"), ("dump", "lsn=1 ")] {
        let mut command = bare();
        command.args([verb, "--data-dir", data]).current_dir(&dir.0).env("HOME", dir.0.join("h"));
        let out = command.stdin(Stdio::null()).output().unwrap();
        let (stdout, stderr) =
            (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.status.code(), Some(0), "{verb}: {stdout}{stderr}");
        assert!(stdout.lines().any(|got| got.starts_with(line)), "{verb}: {stdout}");
    }
}

/// The segment size a home's store gets. New homes get 16 MiB. A store keeps the size it was
/// made with, whatever the config says later.
#[cfg(unix)]
mod segments {
    use super::bare;
    use crate::portable::Dir;
    use crate::support::{atticdb, data_dir, kill_holder, Holders};
    use serde_json::Value;
    use std::fmt::Write;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    /// A fresh `HOME` in `dir`, and its home's data directory.
    fn new_home(dir: &Dir) -> (PathBuf, PathBuf) {
        let home = dir.0.join("h");
        std::fs::create_dir_all(&home).unwrap();
        let data = data_dir(&home);
        (home, data)
    }

    /// Runs a verb on the home, which must succeed. Returns stdout.
    fn run(home: &Path, args: &[&str]) -> String {
        let (code, out, err) = atticdb(home, home, args, &[], b"", 0);
        assert_eq!(code, Some(0), "{args:?}: {err}");
        out
    }

    /// The home's config, parsed.
    fn config(data: &Path) -> Value {
        let path = data.parent().unwrap().join("atticdb.json");
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// Checks there are segments and a spare, each `size` bytes. A spare fill is skipped when
    /// free space moves during it, as when parallel tests delete their stores. Only a later
    /// superblock or a server start retries it, so this restarts the server until it is there.
    fn assert_store_files(home: &Path, data: &Path, size: u64, case: &str) {
        for _ in 0..20 {
            if data.join("spare.zdb").exists() {
                break;
            }
            assert!(kill_holder(data), "{case}: the server stops");
            run(home, &["recent", "--n", "1"]);
        }
        assert!(data.join("spare.zdb").exists(), "{case}: no spare after 20 starts");
        let mut files = Vec::new();
        for entry in std::fs::read_dir(data).unwrap().map(Result::unwrap) {
            let name = entry.file_name().into_string().unwrap();
            if name == "spare.zdb" || (name.starts_with("seg-") && name.ends_with(".zdb")) {
                files.push((name, entry.metadata().unwrap().len()));
            }
        }
        assert!(files.iter().any(|(name, _)| name.starts_with("seg-")), "{case}: {files:?}");
        for (name, len) in &files {
            assert_eq!(*len, size, "{case}: {name}");
        }
    }

    /// The first run makes a home whose config says 16, so its store gets 16 MiB files.
    #[test]
    fn new_home_uses_16_mib_segments() {
        let dir = Dir::new();
        let (home, data) = new_home(&dir);
        let _holders = Holders(vec![data.clone()]);
        run(&home, &["append", "--content", "hi"]);
        assert_eq!(config(&data)["segment_mib"], 16, "the home's config sets 16");
        assert_store_files(&home, &data, 16 << 20, "new home");
    }

    /// A home from before `segment_mib` has 64 MiB files. Setting 16 later changes nothing on
    /// disk. The server keeps the stored size, serves the data and takes appends.
    #[test]
    fn existing_store_keeps_its_segment_size() {
        let dir = Dir::new();
        let (home, data) = new_home(&dir);
        let home_dir = data.parent().unwrap();
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(home_dir).unwrap();
        // `atticdb init` writes what every home got before: tenant `me` and no segment_mib.
        let path = home_dir.join("atticdb.json");
        let init = ["init", "--config", path.to_str().unwrap(), "--tenant", "me"];
        let out = bare().args(init).env("HOME", &home).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(config(&data).get("segment_mib"), None, "an old home's config");
        let _holders = Holders(vec![data.clone()]);
        run(&home, &["append", "--content", "one"]);
        assert_store_files(&home, &data, 64 << 20, "old home");

        assert!(kill_holder(&data), "the server stops");
        let mut edited = config(&data);
        edited["segment_mib"] = 16.into();
        std::fs::write(&path, edited.to_string()).unwrap();
        run(&home, &["append", "--content", "two"]);
        let recent: Value = serde_json::from_str(&run(&home, &["recent", "--n", "10"])).unwrap();
        let turns = recent["turns"].as_array().unwrap();
        let mut found: Vec<_> = turns.iter().filter_map(|turn| turn["content"].as_str()).collect();
        found.sort_unstable();
        assert_eq!(found, ["one", "two"], "{recent}");
        assert_store_files(&home, &data, 64 << 20, "old home with segment_mib 16");
    }

    /// The largest body the server reads, 8 MiB, commits in a 16 MiB home. Each content
    /// character is a 6-byte `\uXXXX` escape that the store keeps as 3 UTF-8 bytes. So the
    /// turns hold about 4 MiB of text that compresses poorly, near the tape's 4 MiB batch cap.
    #[test]
    fn home_accepts_a_body_cap_batch() {
        let dir = Dir::new();
        let (home, data) = new_home(&dir);
        let _holders = Holders(vec![data.clone()]);
        run(&home, &["append", "--content", "hi"]);
        assert_store_files(&home, &data, 16 << 20, "a 16 MiB home");
        let target = (8 << 20) - (16 << 10);
        let skeleton = |a: &str, b: &str| {
            let turn = |content: &str| format!(r#"{{"role":"user","content":"{content}"}}"#);
            let (a, b) = (turn(a), turn(b));
            format!(r#"{{"agent":"a","session":"s","idem_key":"big","turns":[{a},{b}]}}"#)
        };
        let chars = (target - skeleton("", "").len()) / 6;
        // xorshift64 over U+0800 to U+D7FF: 3 UTF-8 bytes each, no surrogates.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut text = || {
            let mut out = String::with_capacity(chars / 2 * 6);
            for _ in 0..chars / 2 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                write!(out, "\\u{:04x}", 0x800 + state % (0xd800 - 0x800)).unwrap();
            }
            out
        };
        let (first, second) = (text(), text());
        let body = skeleton(&first, &second);
        assert!(body.len() > (8 << 20) - (32 << 10) && body.len() <= 8 << 20, "{}", body.len());

        let token = config(&data)["tenants"][0]["token"].as_str().unwrap().to_owned();
        let auth = format!("Bearer {token}");
        let headers = [("Host", "127.0.0.1"), ("Authorization", auth.as_str())];
        let mut stream =
            std::os::unix::net::UnixStream::connect(data.join("atticdb.sock")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let response = atticdb::http::exchange(
            &mut stream,
            "POST",
            "/v1/append",
            &headers,
            body.as_bytes(),
            deadline,
            1 << 20,
        )
        .unwrap();
        let reply: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!((response.status, &reply["outcome"]), (200, &"committed".into()), "{reply}");
        assert_eq!(reply["seqs"], serde_json::json!([1, 2]), "{reply}");
        assert_store_files(&home, &data, 16 << 20, "after the big batch");
    }

    /// `atticdb init`, for servers, writes exactly what it wrote before. With no segment_mib,
    /// a server's new store keeps 64 MiB.
    #[test]
    fn init_output_is_unchanged() {
        let dir = Dir::new();
        let path = dir.0.join("c.json");
        let init = ["init", "--config", path.to_str().unwrap(), "--tenant", "a"];
        let out = bare().args(init).env("HOME", dir.0.join("h")).stdin(Stdio::null()).output();
        let out = out.unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let token = String::from_utf8(out.stdout).unwrap();
        let token = token.strip_suffix('\n').unwrap();
        let want = format!(
            "{{\n  \"tenants\": [\n    {{\n      \"name\": \"a\",\n      \"token\": \"{token}\"\n    \
             }}\n  ]\n}}\n"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
    }
}
