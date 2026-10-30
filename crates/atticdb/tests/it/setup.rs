use crate::support::{self, data_dir, fake_program, tree, tree_outside, Dir, Holders, Process};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// VS Code's `mcp.json` when `HOME` is `home`.
fn vscode_file(home: &Path) -> PathBuf {
    let profile = if cfg!(target_os = "macos") { "Library/Application Support" } else { ".config" };
    home.join(profile).join("Code/User/mcp.json")
}

/// Fakes of the 4 harnesses' programs under `dir`, never the real ones: `claude` and `codex`
/// answer as the real ones do, `copilot` and `code` only log. Returns the `PATH` that holds
/// them, and the argv logs of `claude` and `codex`.
fn fakes(dir: &Path) -> (OsString, PathBuf, PathBuf) {
    let (claude, codex, other) = (dir.join("claude"), dir.join("codex"), dir.join("other"));
    let claude_log = support::fake_claude(&claude);
    let codex_log = support::fake_codex(&codex);
    std::fs::create_dir(&other).unwrap();
    fake_program(&other, "copilot");
    fake_program(&other, "code");
    (std::env::join_paths([claude, codex, other]).unwrap(), claude_log, codex_log)
}

/// The args each run of a fake logged to `log`.
fn calls(log: &Path) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// Runs `atticdb setup` with `args` in `cwd`, with `HOME` set to `home`, `PATH` to `path`, and
/// `env` added. Writes `stdin`, then closes it. Returns the exit code, stdout and stderr.
fn setup(
    home: &Path,
    cwd: &Path,
    path: &OsString,
    env: &[(&str, &str)],
    args: &[&str],
    stdin: &str,
) -> (Option<i32>, String, String) {
    atticdb("setup", home, cwd, path, env, args, stdin)
}

/// [`setup`] for any `verb`.
fn atticdb(
    verb: &str,
    home: &Path,
    cwd: &Path,
    path: &OsString,
    env: &[(&str, &str)],
    args: &[&str],
    stdin: &str,
) -> (Option<i32>, String, String) {
    let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
    command.arg(verb).args(args).current_dir(cwd).env("HOME", home).env("PATH", path);
    command.env_remove("ATTICDB_TOKEN").env_remove("ATTICDB_SETUP_INTERACTIVE");
    command.envs(env.iter().copied()).stdin(Stdio::piped());
    let mut process = Process::start(&mut command);
    let mut input = process.child.stdin.take().unwrap();
    let _ = input.write_all(stdin.as_bytes());
    drop(input);
    let (status, _) = process.wait();
    (status.code(), process.stdout(), process.stderr())
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// With all 4 apps found, `--dry-run` prints 1 line per file and part and writes nothing, nor
/// runs any harness program. `--yes` connects all 4 in order, with `claude mcp add` once and
/// `codex mcp get` only, then checks each: parts in place, the hook answers, the store
/// answers. Codex's guide goes in its AGENTS.md. A rerun changes nothing and says so, the
/// guide included. A Claude part that can't be applied is
/// skipped, and the exit code is 1.
#[test]
fn setup_plans_then_connects_every_app_it_finds() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir(&home).unwrap();
    let (path, claude_log, codex_log) = fakes(&dir.0);
    let _holders = Holders(vec![data_dir(&home)]);
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
    let exe = exe.to_str().unwrap().to_owned();
    let run = |args: &[&str]| setup(&home, &dir.0, &path, &[], args, "");
    let (copilot, vscode) = (home.join(".copilot"), vscode_file(&home));
    let (state, settings) = (home.join(".claude.json"), home.join(".claude/settings.json"));
    let (config, hooks) = (home.join(".codex/config.toml"), home.join(".codex/hooks.json"));
    let agents = home.join(".codex/AGENTS.md");
    let (c, v, h) = (copilot.display(), vscode.display(), home.display());

    let (code, out, err) = run(&["--dry-run", "--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let plan = format!(
        "Plan:\nCopilot CLI:\n  {c}/mcp-config.json: add the MCP entry\n  \
         {c}/hooks/atticdb.json: add the sessionStart hook\n  \
         {c}/instructions/atticdb.instructions.md: add the atticdb instructions\n\
         VS Code:\n  {v}: add the MCP entry\n  VS Code's Copilot chat reads the Copilot hook and \
         instructions in {c}, so VS Code gets those too.\n\
         Claude Code:\n  {h}/.claude.json: add the MCP entry\n  \
         {h}/.claude/settings.json: add the SessionStart hook\n  \
         {h}/.claude/settings.json: add the allow rules\n\
         Codex:\n  {h}/.codex/config.toml: add the MCP entry\n  \
         {h}/.codex/hooks.json: add the SessionStart hook\n  \
         {h}/.codex/AGENTS.md: add the atticdb guide\n\
         Dry run: nothing was written.\n"
    );
    assert_eq!(out, plan);
    assert!(tree(&home).is_empty(), "the dry run wrote nothing, not even the home");
    assert!(calls(&claude_log).is_empty() && calls(&codex_log).is_empty(), "no claude or codex");
    let others = dir.0.join("other");
    assert!(!others.join("copilot.argv").exists() && !others.join("code.argv").exists());

    let (code, out, err) = run(&["--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let applying = out.split("Applying:\n").nth(1).unwrap();
    let lines: Vec<&str> = applying.lines().collect();
    assert_eq!(lines[0], "Copilot CLI:", "{out}");
    assert_eq!(lines[1], format!("  connected atticdb to copilot: {c}/mcp-config.json"));
    assert!(applying.contains(&format!("\nClaude Code:\n  MCP entry: added ({h}/.claude.json)\n")));
    assert!(applying.contains(&format!("\nCodex:\n  MCP entry: added ({h}/.codex/config.toml)\n")));
    assert!(applying.contains("\n4 connected, 0 with skipped parts.\nChecks:\n"), "{out}");
    for app in ["Copilot CLI", "VS Code", "Claude Code", "Codex"] {
        assert!(applying.contains(&format!("\n  {app}: ok\n")), "{app}: {out}");
    }
    let next = "Next:\n  Open Codex once and approve the atticdb hook; Codex runs a new hook only \
                after that.\n  Start new chat sessions to load memory.\n  VS Code: reload the \
                window to load atticdb.\n";
    assert!(out.ends_with(next), "{out}");
    assert_eq!(read(&vscode)["servers"]["atticdb"]["command"], json!(exe));
    assert_eq!(
        read(&copilot.join("mcp-config.json"))["mcpServers"]["atticdb"]["command"],
        json!(exe)
    );
    assert!(copilot.join("hooks/atticdb.json").is_file());
    assert!(copilot.join("instructions/atticdb.instructions.md").is_file());
    assert_eq!(read(&state)["mcpServers"]["atticdb"]["command"], json!(exe));
    let allow = read(&settings)["permissions"]["allow"].clone();
    assert!(allow.as_array().unwrap().contains(&json!("mcp__atticdb__memory_append")));
    assert_eq!(read(&settings)["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    let block = std::fs::read_to_string(&config).unwrap();
    assert!(block.contains("[mcp_servers.atticdb.tools.memory_append]"), "{block}");
    assert_eq!(read(&hooks)["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    let guide = std::fs::read_to_string(&agents).unwrap();
    assert!(guide.contains(&format!("## AtticDB memory\n{}\n", atticdb::mcp::INSTRUCTIONS)));
    let add = ["mcp", "add", "-s", "user", "atticdb", "--", &exe, "mcp", "--agent", "claude-code"];
    assert_eq!(calls(&claude_log), [add.map(String::from).to_vec()]);
    // Once after the write, once in the checks. Never a command that changes anything.
    let get = ["mcp", "get", "atticdb", "--json"].map(String::from).to_vec();
    assert_eq!(calls(&codex_log), [get.clone(), get.clone()]);

    let files = [&vscode, &state, &settings, &config, &hooks, &agents];
    let before = (files.map(|file| std::fs::read(file).unwrap()), tree(&copilot));
    let (code, out, err) = run(&["--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let plan = out.split("Applying:\n").next().unwrap();
    assert_eq!(plan.lines().filter(|line| line.ends_with(" unchanged")).count(), 10, "{out}");
    for line in out.lines() {
        let changed = [": add the ", ": update the ", "added", "updated", "connected atticdb to"];
        let changed = [&changed[..], &["removed", "skipped,", "backup"]].concat();
        assert!(!changed.iter().any(|word| line.contains(word)), "{line}");
    }
    assert!(out.contains("\n4 connected, 0 with skipped parts.\n"), "{out}");
    assert!(out.ends_with(next), "the hook is in place, so Codex still needs approving: {out}");
    assert_eq!((files.map(|file| std::fs::read(file).unwrap()), tree(&copilot)), before);
    assert_eq!(calls(&claude_log).len(), 1, "claude ran only for the first add");

    // A comment makes settings.json more than strict JSON, so its 2 parts are skipped.
    let commented = "{\n  // mine\n}\n";
    std::fs::write(&settings, commented).unwrap();
    let (code, out, err) = run(&["--yes", "--harness", "claude-code"]);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(out.contains("\n  SessionStart hook: skipped, "), "{out}");
    assert!(out.contains("\n0 connected, 1 with skipped parts.\n"), "{out}");
    let failed = "\n  Claude Code: 2 failed\n    not in place: SessionStart hook: skipped, ";
    assert!(out.contains(failed), "{out}");
    assert!(out.contains("\n    not in place: allow rules: skipped, "), "{out}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), commented);
}

/// A case, the args, the env, `PATH`, stdin, the exit code, and text stdout or stderr shows.
type Row<'a> =
    (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)], &'a OsString, &'a str, i32, &'a str);

/// The questions take piped answers under `ATTICDB_SETUP_INTERACTIVE=1`, and their defaults
/// when empty. Without a terminal, `--yes` or an answer, or with no app found, setup writes
/// nothing and runs no harness program.
#[test]
fn setup_asks_and_stops_without_answers() {
    let dir = Dir::new();
    let (path, claude_log, codex_log) = fakes(&dir.0);
    let empty = dir.0.join("empty");
    std::fs::create_dir(&empty).unwrap();
    let empty = OsString::from(&empty);
    // Only VS Code's program, so Copilot CLI is found only by its own files.
    let code_only = dir.0.join("code-only");
    std::fs::create_dir(&code_only).unwrap();
    fake_program(&code_only, "code");
    let code_only = OsString::from(&code_only);
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
    let exe = exe.to_str().unwrap().to_owned();
    let terminal = [("ATTICDB_SETUP_INTERACTIVE", "1")];
    let needs = "atticdb setup needs a terminal, or --yes. See atticdb setup --help.";
    let own = "keep the user's instructions, which mention atticdb";
    let config_dir = [("CLAUDE_CONFIG_DIR", "rel")];
    let codex = ["--yes", "--harness", "codex"];
    let dry = ["--dry-run", "--yes", "--harness"];
    let (dry_claude, dry_codex) =
        ([&dry[..], &["claude-code"]].concat(), [&dry[..], &["codex"]].concat());
    let dry_both = [&dry[..], &["claude-code,codex"]].concat();
    let codex_home = [("CODEX_HOME", "rel")];
    let skipped = "\n  SessionStart hook: skipped, ";
    let rows: [Row<'_>; 17] = [
        ("claude-code and codex", &[], &terminal, &path, "claude-code,codex\n\nn\ny\n", 0, ""),
        ("declined", &[], &terminal, &path, "codex\nn\n\nno\n", 0, "N] Nothing changed.\n"),
        ("not a terminal", &[], &[], &path, "claude-code\n\n\ny\n", 2, needs),
        ("end of stdin", &[], &terminal, &path, "claude-code\n", 2, "no answer on stdin"),
        ("3 unknown apps", &[], &terminal, &path, "claude\nvs code\ncodex,\n", 2, "after 3 tries"),
        ("no apps", &["--yes"], &[], &empty, "", 1, "atticdb found no apps"),
        ("own guide", &["--yes", "--harness", "copilot"], &[], &path, "", 0, own),
        ("VS Code only", &["--yes"], &[], &code_only, "", 0, "\n1 connected, 0 with skipped"),
        // Claude Code stops with exit 2 first. Codex then connects, and 2 stays the code.
        ("2 wins", &["--yes", "--harness", "claude-code,codex"], &config_dir, &path, "", 2, ""),
        // The MCP entry is in place, but not the hook, so there is nothing to approve.
        ("Codex hook skipped", &codex, &[], &path, "", 1, "\n  SessionStart hook: skipped, "),
        ("Codex hooks off", &codex, &[], &path, "", 0, "\n  Codex: ok\n"),
        // The user's AGENTS.md mentions AtticDB, so it is kept, and counts as in place.
        ("Codex own guide", &codex, &[], &path, "", 0, "\n  Codex: ok\n"),
        ("help", &["--yes", "--help"], &[], &path, "", 0, "\n  --dry-run         print the plan"),
        // A dry run exits as its plan would: 1 for a skipped part, else an app's stopped code.
        ("dry run skips", &dry_claude, &[], &path, "", 1, skipped),
        ("dry run stops", &dry_both, &codex_home, &path, "", 2, "  stopped: CODEX_HOME rel must"),
        ("home 0755", &dry_codex, &[], &path, "", 1, "must be a directory owned by uid"),
        ("home link", &dry_codex, &[], &path, "", 1, "is a symlink to a path that doesn't exist"),
    ];
    for (row, (case, args, env, path, stdin, wanted, said)) in rows.into_iter().enumerate() {
        let home = dir.0.join(format!("h{row}"));
        std::fs::create_dir(&home).unwrap();
        let _holders = Holders(vec![data_dir(&home)]);
        // The user wrote their own guide, so AtticDB's isn't needed.
        let guides = home.join(".copilot/instructions");
        let guide = "Save what you learn with atticdb's memory_append.\n";
        if case == "own guide" {
            std::fs::create_dir_all(&guides).unwrap();
            std::fs::write(guides.join("x.instructions.md"), guide).unwrap();
        }
        let codex_dir = home.join(".codex");
        if case == "Codex hook skipped" {
            std::fs::create_dir_all(&codex_dir).unwrap();
            std::fs::write(codex_dir.join("hooks.json"), "{\n  // mine\n}\n").unwrap();
        }
        if case == "Codex hooks off" {
            std::fs::create_dir_all(&codex_dir).unwrap();
            std::fs::write(codex_dir.join("config.toml"), "[features]\nhooks = false\n").unwrap();
        }
        let agents = codex_dir.join("AGENTS.md");
        let agents_kept = "Search atticdb before you start.\n";
        if case == "Codex own guide" {
            std::fs::create_dir_all(&codex_dir).unwrap();
            std::fs::write(&agents, agents_kept).unwrap();
        }
        if case == "dry run skips" {
            std::fs::create_dir_all(home.join(".claude")).unwrap();
            std::fs::write(home.join(".claude/settings.json"), "{\n  // mine\n}\n").unwrap();
        }
        // A home that Apply refuses. The plan shows the same refusal.
        let atticdb_home = data_dir(&home).parent().unwrap().to_owned();
        if case == "home 0755" {
            std::fs::create_dir_all(&atticdb_home).unwrap();
            support::chmod(&atticdb_home, 0o755);
        }
        if case == "home link" {
            std::fs::create_dir_all(atticdb_home.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(home.join("missing"), &atticdb_home).unwrap();
        }
        let before = tree(&home);
        let (code, out, err) = setup(&home, &dir.0, path, env, args, stdin);
        assert_eq!(code, Some(wanted), "{case}: {out}{err}");
        assert!(out.contains(said) || err.contains(said), "{case}: {out}{err}");
        if case == "own guide" {
            let (plan, applying) = out.split_once("Applying:\n").unwrap();
            let user = guides.join("x.instructions.md");
            let user = user.display();
            assert!(plan.contains(&format!("\n  {user}: {own}\n")), "{case}: {out}");
            let kept =
                format!("\n  skipped the atticdb instructions: {user} already mentions atticdb\n");
            assert!(applying.contains(&kept), "{case}: {out}");
            assert!(out.contains("\n1 connected, 0 with skipped parts.\n"), "{case}: {out}");
            assert!(out.contains("\n  Copilot CLI: ok\n"), "{case}: {out}");
            assert_eq!(std::fs::read_to_string(guides.join("x.instructions.md")).unwrap(), guide);
            assert!(!guides.join("atticdb.instructions.md").exists(), "{case}");
            continue;
        }
        if case == "Codex own guide" {
            let (plan, applying) = out.split_once("Applying:\n").unwrap();
            let shown = agents.display();
            let keep = format!("\n  {shown}: keep the user's guide, which mentions atticdb\n");
            assert!(plan.contains(&keep), "{case}: {out}");
            let kept = format!("\n  atticdb guide: kept, {shown} already mentions atticdb\n");
            assert!(applying.contains(&kept), "{case}: {out}");
            assert!(out.contains("\n1 connected, 0 with skipped parts.\n"), "{case}: {out}");
            assert_eq!(std::fs::read_to_string(&agents).unwrap(), agents_kept, "{case}");
            continue;
        }
        if case.starts_with("Codex hook") {
            let (_, next) = out.split_once("\nNext:\n").unwrap();
            assert!(!next.contains("approve"), "{case}: {out}");
            assert!(next.contains("  Start new chat sessions to load memory.\n"), "{case}: {out}");
            let off =
                "  Codex: hooks are off ([features] hooks = false in config.toml), so memory \
                       won't load at chat start. Remove that line to turn them on.\n";
            assert_eq!(next.contains(off), case == "Codex hooks off", "{case}: {out}");
            assert!(codex_dir.join("config.toml").is_file(), "{case}");
            continue;
        }
        if case == "2 wins" {
            let stopped =
                "\n  failed: CLAUDE_CONFIG_DIR rel must be an absolute path. Nothing changed\n";
            assert!(out.contains(stopped), "{case}: {out}");
            assert!(out.contains("\n1 connected, 1 with skipped parts.\n"), "{case}: {out}");
            assert!(out.contains("\n  Codex: ok\n"), "{case}: {out}");
            assert!(home.join(".codex/config.toml").is_file() && !home.join(".claude").exists());
            continue;
        }
        if case == "VS Code only" {
            // VS Code wrote the shared hook in ~/.copilot. That is not Copilot CLI's config.
            assert!(home.join(".copilot/hooks/atticdb.json").is_file(), "{case}");
            let (code, out, err) = setup(&home, &dir.0, path, env, args, stdin);
            assert_eq!(code, Some(0), "{case}: again: {out}{err}");
            assert!(out.starts_with("Plan:\nVS Code:\n"), "{case}: {out}");
            assert!(!out.contains("Copilot CLI"), "{case}: {out}");
            assert!(!home.join(".copilot/mcp-config.json").exists(), "{case}");
            continue;
        }
        if case == "help" {
            assert!(out.starts_with("usage: atticdb setup [--yes] [--harness <list>]"), "{out}");
            assert!(err.is_empty(), "{case}: {err}");
        }
        if case.starts_with("home ") {
            let uid = rustix::process::geteuid().as_raw();
            let refused = match case {
                "home link" => {
                    format!(
                        "{} is a symlink to a path that doesn't exist\n",
                        atticdb_home.display()
                    )
                }
                _ => format!(
                    "{} must be a directory owned by uid {uid}, mode 0700 or stricter, found mode \
                     0755 owned by uid {uid}\n",
                    atticdb_home.display()
                ),
            };
            assert!(out.contains(&format!("\n  stopped: {refused}")), "{case}: {out}");
            let (code, out, err) = setup(&home, &dir.0, path, env, &codex[..], stdin);
            assert_eq!(code, Some(1), "{case}: applied: {out}{err}");
            assert!(out.contains(&format!("\n  failed: {refused}")), "{case}: {out}");
        }
        if row > 0 {
            assert_eq!(tree(&home), before, "{case}: nothing written");
            continue;
        }
        let asked = "Which apps? [copilot,vscode,claude-code,codex] Load memory at the start of \
                     each chat? [Y/n] Save notes without asking? (Claude Code and Codex. Saving \
                     a rule always asks.) [Y/n] Plan:\nClaude Code:\n";
        assert!(out.starts_with(asked), "{case}: {out}");
        assert!(out.contains("Apply? [y/N] Applying:\n"), "{case}: {out}");
        assert!(out.contains("\n2 connected, 0 with skipped parts.\n"), "{case}: {out}");
        assert!(out.contains("\n  Claude Code: ok\n  Codex: ok\n"), "{case}: {out}");
        let next = "Next:\n  Open Codex once and approve the atticdb hook; Codex runs a new hook \
                    only after that.\n  Start new chat sessions to load memory.\n";
        assert!(out.ends_with(next), "{case}: {out}");
        let settings = read(&home.join(".claude/settings.json"));
        let allow = settings["permissions"]["allow"].as_array().unwrap().clone();
        assert_eq!(allow.len(), 4, "{case}: reads only, notes ask: {allow:?}");
        assert!(!allow.contains(&json!("mcp__atticdb__memory_append")), "{case}");
        assert_eq!(settings["hooks"]["SessionStart"].as_array().unwrap().len(), 1, "{case}");
        assert_eq!(
            read(&home.join(".claude.json"))["mcpServers"]["atticdb"]["command"],
            json!(exe)
        );
        let block = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(block.contains("[mcp_servers.atticdb]"), "{case}: {block}");
        assert!(!block.contains("memory_append"), "{case}: notes ask: {block}");
        assert!(home.join(".codex/hooks.json").is_file(), "{case}");
        assert!(!home.join(".copilot").exists() && !vscode_file(&home).exists(), "{case}");
    }
    let adds = calls(&claude_log).into_iter().filter(|args| args[1] != "get").count();
    assert_eq!(adds, 1, "claude ran only to add the first row's entry");
    assert!(calls(&codex_log).iter().all(|args| args[..2] == ["mcp", "get"]), "codex only read");
}

/// A server that holds the data lock but answers only after 8 s, as a slow cold start does,
/// still passes the store check: setup waits for it instead of failing with no trusted
/// server in time.
#[test]
fn setup_store_check_waits_for_a_starting_server() {
    let dir = Dir::new();
    let (path, _, _) = fakes(&dir.0);
    let home = dir.0.join("h");
    let data = data_dir(&home);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&data).unwrap();
    // This process is the starting server: it holds the lock, so every spawned server exits
    // 75. Its pid is this test's, so the lock is released here, not by `Holders`.
    let mut lock = File::create(data.join("LOCK")).unwrap();
    lock.try_lock().unwrap();
    let socket = data.join("atticdb.sock");
    let holder =
        format!("pid={} instance=X unix={} tcp=none\n", std::process::id(), socket.display());
    lock.write_all(holder.as_bytes()).unwrap();
    let late = socket.clone();
    // Started first, so the server can't come up less than 8 s after it.
    let started = Instant::now();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(8));
        support::slow_store(&late, Some(Duration::ZERO));
    });
    let (code, out, err) = setup(&home, &dir.0, &path, &[], &["--yes", "--harness", "codex"], "");
    let took = started.elapsed();
    drop(lock);
    support::kill_holder(&data);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("\n  Codex: ok\n"), "{out}");
    assert!(took >= Duration::from_secs(8), "answered only by the late server: {took:?}");
}

/// An AtticDB binary that doesn't exist.
const OLD: &str = "/old/atticdb";
/// What doctor says about a home that isn't set up.
const NOT_SET_UP: &str = "not set up yet, run atticdb setup\n";
/// Codex's line when `codex mcp get atticdb --json` shows this AtticDB.
const GET_OK: &str = "  codex mcp get atticdb: ok\n";

/// A case, `HOME`, `ATTICDB_HOME`, `PATH`, the args, the exit code, and text stdout or stderr
/// shows.
type DoctorRow<'a> =
    (&'a str, &'a Path, Option<&'a Path>, &'a OsString, &'a [&'a str], i32, String);

/// `atticdb doctor` on a home that setup connected to all 4 fake apps, then with 1 thing
/// wrong per row. A failed check, or a home that isn't set up, exits 1. A warning, or an app
/// not found, exits 0. It runs only `codex mcp get`, and writes nothing in `HOME` or the home
/// but the server's own files in `data/`.
#[test]
fn doctor_checks_an_install_and_writes_nothing() {
    let dir = Dir::new();
    let (path, claude_log, codex_log) = fakes(&dir.0);
    let (home, bare) = (dir.0.join("h"), dir.0.join("n"));
    let no_apps = OsString::from(dir.0.join("empty"));
    for made in [&home, &bare, &dir.0.join("empty")] {
        std::fs::create_dir(made).unwrap();
    }
    let synced = dir.0.join("Dropbox/atticdb");
    let (empty_home, no_data, bad) = (dir.0.join("e"), dir.0.join("p"), dir.0.join("bad"));
    for made in [&synced, &empty_home, &no_data.join("data"), &bad.join("data")] {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(made).unwrap();
    }
    std::fs::remove_dir(no_data.join("data")).unwrap();
    // A dangling atticdb.json link, and a data link to a real, empty dir.
    let (config_link, data_link, real) = (dir.0.join("cl"), dir.0.join("dl"), dir.0.join("rd"));
    for made in [&config_link.join("data"), &data_link, &real] {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(made).unwrap();
    }
    std::os::unix::fs::symlink(dir.0.join("missing"), config_link.join("atticdb.json")).unwrap();
    std::os::unix::fs::symlink(&real, data_link.join("data")).unwrap();
    for config in [&no_data, &bad, &data_link].map(|home| home.join("atticdb.json")) {
        std::fs::write(&config, "{").unwrap();
        support::chmod(&config, 0o600);
    }
    let _holders = Holders(vec![data_dir(&home), synced.join("data")]);
    let (code, out, err) = setup(&home, &dir.0, &path, &[], &["--yes"], "");
    assert_eq!(code, Some(0), "{out}{err}");
    // A read sets up the synced home.
    let env = [("ATTICDB_HOME", synced.to_str().unwrap())];
    let list = ["list", "--prefix", "x"];
    let (code, out, err) = atticdb("fact", &bare, &dir.0, &no_apps, &env, &list, "");
    assert_eq!(code, Some(0), "{out}{err}");
    let no_codex = std::env::join_paths([dir.0.join("claude"), dir.0.join("other")]).unwrap();
    let link = dir.0.join("atticdb-link");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_atticdb"), &link).unwrap();
    let (claude_calls, codex_calls) = (calls(&claude_log), calls(&codex_log));
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
    let (vscode, agents) = (vscode_file(&home), home.join(".codex/AGENTS.md"));
    let atticdb_home = data_dir(&home).parent().unwrap().to_owned();
    let (z, v, n) = (atticdb_home.display(), vscode.display(), data_dir(&bare));
    let n = n.parent().unwrap().display().to_string();
    let uid = rustix::process::geteuid().as_raw();
    let fix = |name: &str| format!("run atticdb setup --harness {name}, or atticdb connect {name}");
    let (shared_hook, version) =
        (home.join(".copilot/hooks/atticdb.json"), env!("CARGO_PKG_VERSION"));
    let sandbox = "Sandboxes:\n  Agents in a sandbox need read and write access to";
    let unset =
        format!("Home:\n  {n} (ATTICDB_HOME is not set): not set up yet, run atticdb setup\n");
    let unset = format!("{unset}{sandbox} {n}.\n");
    let healthy = format!("atticdb:\n  version {version}, {}: ok\nHome:\n  {z} (", exe.display());
    let healthy = format!("{healthy}ATTICDB_HOME is not set): ok\n");
    let guide =
        format!("\nCodex:\n  parts: 1 would change, {}\n    {}", fix("codex"), agents.display());
    let guide = format!("{guide}: add the atticdb guide\n  binary: ok\n  hook: ok\n{GET_OK}");
    let hook = shared_hook.display();
    let (settings, toml) = (home.join(".claude/settings.json"), home.join(".codex/config.toml"));
    let gone =
        |file: &Path| format!("    {} runs /old/atticdb, which no longer exists\n", file.display());
    let stale = [
        format!("VS Code:\n  parts: 2 would change, {}\n", fix("vscode")),
        format!("    {v}: update the MCP entry\n    {hook}: update the sessionStart hook\n"),
        format!("  binary: stale, {}\n{}", fix("vscode"), gone(&vscode)),
        format!("    {hook} runs /bin/sh, not this atticdb, {}\n", exe.display()),
        // The hook would change, so it isn't taken as left out.
        format!("Claude Code:\n  parts: 1 would change, {}\n", fix("claude-code")),
        format!("    {}: update the SessionStart hook\n", settings.display()),
        format!("  binary: stale, {}\n{}", fix("claude-code"), gone(&settings)),
        format!("Codex:\n  parts: 1 would change, {}\n", fix("codex")),
        format!("    {}: update the MCP entry\n", toml.display()),
        format!("  binary: stale, {}\n{}  hook: ok\n", fix("codex"), gone(&toml)),
        format!(
            "  codex mcp get atticdb: codex mcp get atticdb --json does not show {}",
            json!(exe)
        ),
        format!(" mcp --agent codex\n{sandbox}"),
    ];
    let mode =
        format!("  {z} (ATTICDB_HOME is not set): {z} must be a directory owned by uid {uid}");
    let mode = format!("{mode}, mode 0700 or stricter, found mode 0755 owned by uid {uid}\n");
    let moved = Some(synced.as_path());
    let config = format!("\nServer and store: config {}/atticdb.json: invalid JSON", bad.display());
    let moved_ok = format!("  {} (set by ATTICDB_HOME): ok\n", synced.display());
    let moved_unset =
        |home: &Path| format!("  {} (set by ATTICDB_HOME): {NOT_SET_UP}", home.display());
    let both = ["--harness", "copilot,bogus"];
    let stale_apps = ["--harness", "codex,vscode,claude-code"];
    let (codex, copilot) = (["--harness", "codex"], ["--harness", "copilot"]);
    let get_failed = "\n  codex mcp get atticdb: codex mcp get atticdb --json failed";
    let not_verified = "\n  hook: ok\n  codex mcp get atticdb: not verified: codex not found\n";
    let off =
        format!("\n  note: {} sets hooks = false in [features], so Codex runs", toml.display());
    let linked = format!("Copilot CLI:\n  parts: 1 would change, {}\n    ", fix("copilot"));
    let entry = home.join(".copilot/mcp-config.json");
    let linked =
        format!("{linked}{}: update the MCP entry\n  binary: ok\n  hook: ok\n", entry.display());
    let odd = |home: &Path, name: &str, kind: &str| {
        let path = home.join(name);
        format!("{} is a symlink or not a {kind}; fix it by hand\n", path.display())
    };
    let config_odd = odd(&config_link, "atticdb.json", "regular file");
    let data_odd = odd(&data_link, "data", "directory");
    let rows: [DoctorRow<'_>; 16] = [
        ("not set up", &bare, None, &no_apps, &[], 1, unset),
        (
            "empty ATTICDB_HOME",
            &bare,
            Some(&empty_home),
            &no_apps,
            &[],
            1,
            moved_unset(&empty_home),
        ),
        ("no data dir", &bare, Some(&no_data), &no_apps, &[], 1, moved_unset(&no_data)),
        ("config link", &bare, Some(&config_link), &no_apps, &[], 1, config_odd),
        ("data link", &bare, Some(&data_link), &no_apps, &[], 1, data_odd),
        ("healthy", &home, None, &path, &[], 0, healthy),
        ("part would change", &home, None, &path, &[], 1, guide),
        ("stale binary", &home, None, &path, &stale_apps, 1, stale.concat()),
        ("home 0755", &home, None, &path, &[], 1, mode),
        ("unknown harness", &home, None, &path, &both, 2, "usage:".into()),
        ("synced folder", &bare, moved, &no_apps, &[], 0, moved_ok),
        ("bad config", &bare, Some(&bad), &no_apps, &[], 1, config),
        ("codex bad toml", &home, None, &path, &codex, 1, get_failed.into()),
        ("codex not on PATH", &home, None, &no_codex, &codex, 0, not_verified.into()),
        ("codex hooks off", &home, None, &path, &codex, 0, off),
        ("symlinked binary", &home, None, &path, &copilot, 1, linked),
    ];
    let shared = &shared_hook;
    for (case, row_home, moved, path, args, wanted, said) in rows {
        let files = [&agents, &vscode, shared, &settings, &toml, &entry];
        let saved = files.map(|file| std::fs::read(file).unwrap());
        match case {
            "part would change" => std::fs::remove_file(&agents).unwrap(),
            // A binary that is gone, and one that exists but isn't this one.
            "stale binary" => {
                let mut config = read(&vscode);
                config["servers"]["atticdb"]["command"] = json!(OLD);
                std::fs::write(&vscode, config.to_string()).unwrap();
                let exe = exe.to_str().unwrap();
                for (file, other) in [(shared, "/bin/sh"), (&settings, OLD), (&toml, OLD)] {
                    let text = std::fs::read_to_string(file).unwrap();
                    std::fs::write(file, text.replace(exe, other)).unwrap();
                }
            }
            // Valid for AtticDB's scanner, but not TOML, so only codex finds it.
            "codex bad toml" => {
                std::fs::write(&toml, [b"model = ???\n", &saved[4][..]].concat()).unwrap()
            }
            "codex hooks off" => {
                std::fs::write(&toml, [b"[features]\nhooks = false\n\n", &saved[4][..]].concat())
                    .unwrap()
            }
            "symlinked binary" => {
                let mut config = read(&entry);
                config["mcpServers"]["atticdb"]["command"] = json!(link.to_str().unwrap());
                std::fs::write(&entry, config.to_string()).unwrap();
            }
            "home 0755" => support::chmod(&atticdb_home, 0o755),
            _ => {}
        }
        // HOME and the home it names, or ATTICDB_HOME, each without the server's `data/`.
        let mut homes = vec![(row_home.to_owned(), data_dir(row_home))];
        homes.extend(moved.map(|moved| (moved.to_owned(), moved.join("data"))));
        let own = || homes.iter().map(|(home, data)| tree_outside(home, Some(data))).collect();
        let before: Vec<_> = own();
        let moved = moved.map(|moved| ("ATTICDB_HOME", moved.to_str().unwrap()));
        let env: Vec<_> = moved.into_iter().collect();
        let (code, out, err) = atticdb("doctor", row_home, &dir.0, path, &env, args, "");
        assert_eq!(code, Some(wanted), "{case}: {out}{err}");
        assert!(out.contains(&said) || err.contains(&said), "{case}: {out}{err}");
        assert_eq!(own(), before, "{case}: nothing written");
        assert_eq!(out.contains(sandbox), wanted != 2, "{case}: {out}");
        match case {
            "not set up" => assert!(tree(&bare).is_empty(), "{case}: no home made"),
            "config link" | "data link" => {
                let skipped = "\nServer and store: skipped, fix the home first\nSandboxes:\n";
                assert!(out.contains(skipped), "{case}: {out}");
                assert!(tree(&real).is_empty(), "{case}: nothing under the link");
                let link = std::fs::read_link(data_link.join("data")).unwrap();
                assert_eq!(link, real, "{case}: the data link stays");
            }
            "empty ATTICDB_HOME" | "no data dir" => {
                let skipped = "\nServer and store: skipped, set it up first\nSandboxes:\n";
                assert!(out.contains(skipped), "{case}: {out}");
                assert!(tree(&empty_home).is_empty(), "{case}: no config, token or data/");
                assert!(!no_data.join("data").exists(), "{case}: no data/");
            }
            "codex bad toml" => assert!(!out.contains("Error loading"), "{case}: {out}"),
            "codex hooks off" => {
                let warned = "\n  hook: warning: Codex's hooks are off, so memory won't load at \
                              chat start. Turn them on in [features] in config.toml: remove \
                              hooks = false, or set hooks = true\n";
                assert!(out.contains(warned), "{case}: {out}");
                assert!(!out.contains("hook: ok"), "{case}: {out}");
            }
            "healthy" => {
                for app in ["Copilot CLI", "VS Code", "Claude Code", "Codex"] {
                    let get = if app == "Codex" { GET_OK } else { "" };
                    let ok = format!("\n{app}:\n  parts: ok\n  binary: ok\n  hook: ok\n{get}");
                    assert!(out.contains(&ok), "{case}: {app}: {out}");
                }
                let lines: Vec<&str> = out.lines().collect();
                assert!(lines[4].starts_with("  free space: "), "{case}: {out}");
                assert!(lines[4].contains(" free, the floor is "), "{case}: {out}");
                assert!(lines[5].starts_with("  filesystem: "), "{case}: {out}");
                assert!(lines[5].ends_with(": ok"), "{case}: {out}");
                assert_eq!(lines[6..8], ["  synced folder: ok", "Server and store: ok"]);
            }
            "stale binary" => assert!(!out.contains("Copilot CLI"), "{case}: {out}"),
            "home 0755" => {
                assert!(out.contains("\nServer and store: skipped, fix the home first\n"));
                assert!(!out.contains("Codex"), "{case}: {out}");
                let mode = std::fs::metadata(&atticdb_home).unwrap().permissions();
                assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o755);
                support::chmod(&atticdb_home, 0o700);
            }
            "synced folder" => {
                let warned = "\n  synced folder: warning: the home is in Dropbox, which may copy \
                              the store while it is written. Keep the home outside synced \
                              folders, with ATTICDB_HOME\nServer and store: ok\n";
                assert!(out.contains(warned), "{case}: {out}");
                assert!(out.contains("\nCodex: not found\nSandboxes:\n"), "{case}: {out}");
                assert!(tree(&bare).is_empty(), "{case}: HOME untouched");
            }
            _ => {}
        }
        for (file, bytes) in files.into_iter().zip(saved) {
            std::fs::write(file, bytes).unwrap();
        }
    }
    assert_eq!(calls(&claude_log), claude_calls, "claude never ran");
    let gets = &calls(&codex_log)[codex_calls.len()..];
    assert!(gets.iter().all(|args| *args == ["mcp", "get", "atticdb", "--json"]), "{gets:?}");
    let others = dir.0.join("other");
    assert!(!others.join("copilot.argv").exists() && !others.join("code.argv").exists());
}
