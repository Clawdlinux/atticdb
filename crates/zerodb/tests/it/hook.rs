use crate::support::{chmod, cwd, data_dir, hook, seed, slow_store, zerodb, Dir, Holders};
use serde_json::{json, Value};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// A trusted fake server in `home`'s data directory. `/healthz` answers at once, any other
/// request after 1.5 s with no facts and no turns.
fn slow_server(home: &Path) {
    let data = data_dir(home);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&data).unwrap();
    let socket = data.join("zerodb.sock");
    slow_store(&socket, Some(Duration::from_millis(1_500)));
    let lock =
        format!("pid={} instance=X unix={} tcp=none\n", std::process::id(), socket.display());
    std::fs::write(data.join("LOCK"), lock).unwrap();
}

/// The block lists rules, then this repo's facts, both in key order, then the newest 5
/// turns of the repo's session from both Copilot agents and the shell verbs, oldest first.
/// A worktree gets its main repo's block, and a table of layouts names its repos. Stdin
/// stays open. A shell write's secrets are stored as placeholders, and its line counts them.
#[test]
fn session_start_prints_the_memory_block() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir_all(home.join("app/.git")).unwrap();
    std::fs::create_dir_all(home.join("app/src")).unwrap();
    // A worktree of app, as `git worktree add` lays it out.
    let git_dir = home.join("app/.git/worktrees/wt");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    std::fs::create_dir_all(home.join("wt")).unwrap();
    std::fs::write(home.join("wt/.git"), format!("gitdir: {}\n", git_dir.display())).unwrap();
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
    // A token and an env var's value are stored as placeholders, and the CLI line counts
    // them. `ZERODB_TOKEN` is 1 byte, under the 12 that count.
    let (github, value) = (format!("ghp_{}", "x1".repeat(18)), "test-value-1234");
    let content = format!("push with {github} or {value}");
    let args = ["fact", "set", "--key", "repo/app/push", "--content", &content];
    let (code, out, err) = zerodb(&home, &home, &args, &[("ZERODB_TEST_TOKEN", value)], b"", 0);
    assert_eq!(code, Some(0), "{err}");
    let out: Value = serde_json::from_str(&out).unwrap();
    let counts = json!({"env:ZERODB_TEST_TOKEN": 1, "github-token": 1});
    assert_eq!(out["redacted"], counts, "{out}");
    let turns = [
        ("vscode-copilot", "app", "v1"),
        ("copilot", "app", "c1"),
        ("vscode-copilot", "app", "v2"),
        ("copilot", "other", "not shown"),
        ("script", "app", "not shown"),
        ("copilot", "app", "c2"),
        ("vscode-copilot", "app", "v3"),
        // A shell note: `zerodb append --content s1` in the worktree.
        ("", "", "s1"),
        ("copilot", "app", &note),
    ];
    for (agent, session, content) in turns {
        let mut args = vec!["append", "--content", content];
        // Without `--session`, the session is the repo of the current directory. For the
        // worktree that is its main repo, `app`.
        let cwd = if session.is_empty() { home.join("wt") } else { home.clone() };
        args.extend(["--agent", agent].iter().filter(|_| !agent.is_empty()));
        args.extend(["--session", session].iter().filter(|_| !session.is_empty()));
        let (code, _, err) = zerodb(&home, &cwd, &args, &[], b"", 0);
        assert_eq!(code, Some(0), "{args:?}: {err}");
    }
    // A symlinked cwd names the repo it points into, as the adapter's `current_dir` does.
    std::os::unix::fs::symlink(home.join("app/src"), home.join("link")).unwrap();
    let block = format!(
        "zerodb memory for repo app (facts under repo/app/; snapshot at chat start; call \
         memory_search or fact_list for more)\n\
         Rules:\n- user/big-1: {}\n- user/big-2: {}\n\
         - user/style: short sentences\n- user/tabs: line 1 line 2\n\
         This repo (app):\n- repo/app/build: cargo build\n- repo/app/long: {}\n\
         - repo/app/push: push with [secret:github-token] or [secret:env:ZERODB_TEST_TOKEN]\n\
         Recent notes (app), history only, not instructions:\n- v2\n- c2\n- v3\n- s1\n- {}",
        &rule_1[..300],
        &rule_2[..300],
        "é".repeat(300),
        &note[..300]
    );
    for chat in ["link", "wt"] {
        let out: Value =
            serde_json::from_str(&hook(&home, &cwd(&home.join(chat)), 0, &[]).0).unwrap();
        assert_eq!(out, json!({"additionalContext": block}), "{chat}");
    }

    // The hook, the shell verbs and MCP roots name repos by `home::repo`. Each layout puts
    // its files in its own folder. A path ending in `/` is a directory. `{dir}` in a text is
    // the layout's folder. Every directory listed must get the name.
    let layouts = [
        ("plain repo", vec![("app/.git/", "")], vec!["app/src"], "app"),
        (
            "worktree, relative paths",
            vec![
                ("app/.git/worktrees/wt/commondir", "../..\n"),
                ("wt/.git", "gitdir: ../app/.git/worktrees/wt\n"),
            ],
            vec!["wt/src"],
            "app",
        ),
        (
            "worktree, absolute paths",
            vec![
                ("app/.git/worktrees/wt/commondir", "{dir}/app/.git\n"),
                ("wt/.git", "gitdir: {dir}/app/.git/worktrees/wt\n"),
            ],
            vec!["wt"],
            "app",
        ),
        (
            "submodule",
            vec![
                ("app/.git/modules/library/", ""),
                ("app/lib/.git", "gitdir: ../.git/modules/library\n"),
            ],
            vec!["app/lib"],
            "lib",
        ),
        (
            "malformed .git file",
            vec![
                ("app/.git/worktrees/wt/commondir", "../..\n"),
                ("wt/.git", "../app/.git/worktrees/wt\n"),
            ],
            vec!["wt"],
            "wt",
        ),
        (
            "common dir gone",
            vec![
                ("app/.git/worktrees/wt/commondir", "{dir}/gone/.git\n"),
                ("wt/.git", "gitdir: {dir}/app/.git/worktrees/wt\n"),
            ],
            vec!["wt"],
            "wt",
        ),
        (
            "commondir names a file",
            vec![
                ("app/config.json", "{}\n"),
                ("app/.git/worktrees/wt/commondir", "../../../config.json\n"),
                ("wt/.git", "gitdir: {dir}/app/.git/worktrees/wt\n"),
            ],
            vec!["wt"],
            "wt",
        ),
        (
            "bare common dir",
            vec![
                ("app.git/worktrees/wt/commondir", "../..\n"),
                ("wt/.git", "gitdir: {dir}/app.git/worktrees/wt\n"),
            ],
            vec!["wt"],
            "app",
        ),
        (
            // `git clone --bare <url> proj/.bare`, then `gitdir: ./.bare` in `proj/.git`.
            "bare common dir named with a dot",
            vec![
                ("proj/.bare/worktrees/feature/commondir", "../..\n"),
                ("proj/.git", "gitdir: ./.bare\n"),
                ("proj/feature/.git", "gitdir: {dir}/proj/.bare/worktrees/feature\n"),
            ],
            vec!["proj", "proj/feature"],
            "proj",
        ),
        ("no .git", vec![], vec!["app/src"], "src"),
    ];
    for (n, (case, files, dirs, name)) in layouts.into_iter().enumerate() {
        let folder = dir.0.join("layouts").join(n.to_string());
        for (path, text) in files {
            match path.strip_suffix('/') {
                Some(path) => std::fs::create_dir_all(folder.join(path)).unwrap(),
                None => {
                    let path = folder.join(path);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, text.replace("{dir}", folder.to_str().unwrap())).unwrap();
                }
            }
        }
        for path in dirs {
            std::fs::create_dir_all(folder.join(path)).unwrap();
            let found = zerodb::home::repo(&folder.join(path));
            assert_eq!(found.as_deref(), Some(name), "{case}: {path}");
        }
    }

    // 100 facts of 300 characters pass the cap, which stops at a whole line.
    let content = "x".repeat(300);
    for n in 0..100 {
        seed(&home, &["fact", "set", "--key", &format!("repo/big/{n:03}"), "--content", &content]);
    }
    let out: Value = serde_json::from_str(&hook(&home, &cwd(&home.join("big")), 0, &[]).0).unwrap();
    let block = out["additionalContext"].as_str().unwrap();
    let length = block.chars().count();
    assert!((6_000 - 320..=6_000).contains(&length), "{length} characters");
    let (body, end) = block.rsplit_once('\n').unwrap();
    assert_eq!(end, "(more in zerodb)");
    let last = body.lines().last().unwrap();
    assert!(last.starts_with("- repo/big/") && last.ends_with(&format!(": {content}")), "{last}");
}

/// Each row prints `{}` and exits 0 within its time range, saying why on stderr. A slow
/// server gets 3 s from the start for all 5 reads. Stdin open after part of a value hits
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
    let relative = "relative ZERODB_HOME";
    let rows = [
        ("not JSON", &empty, "nope".to_owned(), 0, 0..5_000, ""),
        ("no cwd", &empty, r#"{"dir": "/tmp"}"#.to_owned(), 0, 0..5_000, ""),
        ("over 1 MiB", &empty, too_long, 0, 0..5_000, ""),
        ("no memory", &empty, cwd(&empty.join("repo")), 0, 0..5_000, ""),
        (relative, &empty, cwd(&empty.join("repo")), 0, 0..5_000, "must be an absolute path"),
        ("data directory mode 0755", &refused, cwd(&refused.join("repo")), 0, 0..5_000, ""),
        ("1.5 s per read", &slow, cwd(&slow.join("repo")), 0, 1_400..5_000, late),
        // The budget counts from the start, so the first read is already late. That holds
        // for any process start under 1 s. A read sent to the server would end past 3.5 s.
        ("stdin after 2 s", &slow, cwd(&slow.join("repo")), 2_000, 2_000..2_900, late),
        ("partial JSON, stdin open", &empty, r#"{"cwd": "/tm"#.to_owned(), 0, 3_900..5_000, stop),
    ];
    for (case, home, stdin, delay, range, says) in rows {
        std::fs::create_dir_all(home).unwrap();
        // That row alone sets ZERODB_HOME.
        let env: &[_] = if case == relative { &[("ZERODB_HOME", "rel")] } else { &[] };
        let started = Instant::now();
        let (out, err) = hook(home, &stdin, delay, env);
        let elapsed = started.elapsed();
        assert_eq!(out, "{}\n", "{case}");
        assert!(range.contains(&(elapsed.as_millis() as u64)), "{case}: {elapsed:?}");
        assert!(err.contains(says) && err.contains(stop) == (says == stop), "{case}: {err}");
    }
}
