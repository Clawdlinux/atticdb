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

/// The "Try it" lines a setup that applied with no failure prints last, for the repo the
/// session-start hook reads in `cwd`. `atticdb` is how the lines name the binary.
fn try_it(cwd: &Path, atticdb: &str) -> String {
    let repo = atticdb::home::repo(cwd).unwrap();
    format!(
        "Try it: {atticdb} fact set --key repo/{repo}/rule --content \"Use pnpm, not npm\"\n\
         Then open a new chat and ask: what are this repo's rules?\n\
         See what your agent gets at chat start: {atticdb} studio\n"
    )
}

/// How setup and doctor name the binary under test when `atticdb` on `PATH` is not it: its
/// real path, single-quoted when it has other than letters, digits and `/._+-`.
fn typed_exe() -> String {
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
    let exe = exe.to_str().unwrap();
    if exe.chars().all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c)) {
        exe.to_owned()
    } else {
        format!("'{}'", exe.replace('\'', r"'\''"))
    }
}

/// Makes `dir`, holding only `atticdb`, a symlink to the binary under test. On `PATH`, it
/// makes setup and doctor name the binary `atticdb`.
fn on_path(dir: &Path) -> OsString {
    std::fs::create_dir(dir).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_atticdb"), dir.join("atticdb")).unwrap();
    OsString::from(dir)
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// With all 4 apps found, `--dry-run` prints 1 line per file and part and writes nothing, nor
/// runs any harness program. `--yes` connects all 4 in order, with `claude mcp add` once and
/// `codex mcp get` only, then checks each: parts in place, the hook answers, the store
/// answers. Codex's guide goes in its AGENTS.md. A rerun changes nothing and says so, the
/// guide included. A Claude part that can't be applied is
/// skipped, and the exit code is 1. VS Code reads Copilot CLI's entry, so it lists 1 atticdb
/// server whichever app is connected first.
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
         VS Code:\n  {v}: no atticdb entry needed. VS Code 1.144 and later read it from \
         {c}/mcp-config.json. Older VS Code: update it, or run atticdb connect vscode --force\n  \
         VS Code's Copilot chat reads the Copilot hook and \
         instructions in {c}, so VS Code gets those too.\n\
         Claude Code:\n  {h}/.claude.json: add the MCP entry\n  \
         {h}/.claude/settings.json: add the SessionStart hook\n  \
         {h}/.claude/settings.json: add the PostCompact hook (saves compaction summaries)\n  \
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
    let next = format!(
        "Next:\n  Open Codex once and approve the atticdb hook; Codex runs a new hook only after \
         that.\n  Start new chat sessions to load memory.\n  VS Code: reload the window to load \
         atticdb.\n{}",
        try_it(&dir.0, &typed_exe())
    );
    assert!(out.ends_with(&next), "{out}");
    assert!(!vscode.exists(), "VS Code reads Copilot CLI's entry, so it gets none of its own");
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
    assert_eq!(read(&settings)["hooks"]["PostCompact"].as_array().unwrap().len(), 1);
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

    let files = [&state, &settings, &config, &hooks, &agents];
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
    assert!(out.ends_with(&next), "the hook is in place, so Codex still needs approving: {out}");
    assert_eq!((files.map(|file| std::fs::read(file).unwrap()), tree(&copilot)), before);
    assert_eq!(calls(&claude_log).len(), 1, "claude ran only for the first add");

    // A comment makes settings.json more than strict JSON, so its 3 parts are skipped.
    let commented = "{\n  // mine\n}\n";
    std::fs::write(&settings, commented).unwrap();
    let (code, out, err) = run(&["--yes", "--harness", "claude-code"]);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(out.contains("\n  SessionStart hook: skipped, "), "{out}");
    assert!(out.contains("\n0 connected, 1 with skipped parts.\n"), "{out}");
    let failed = "\n  Claude Code: 3 failed\n    not in place: SessionStart hook: skipped, ";
    assert!(out.contains(failed), "{out}");
    assert!(out.contains("\n    not in place: PostCompact hook: skipped, "), "{out}");
    assert!(out.contains("\n    not in place: allow rules: skipped, "), "{out}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), commented);

    // VS Code reads Copilot CLI's entry too. So it lists exactly 1 atticdb server, whichever
    // app is connected first, in 1 run or 2. The plan says what the run does to mcp.json.
    // (case, each run's apps and what its output says of mcp.json, "" for nothing)
    let rows: [(&str, &[(&str, &str)]); 3] = [
        ("1 run, vscode named first", &[("vscode,copilot", "no atticdb entry needed")]),
        ("vscode, then copilot", &[("vscode", "add the MCP entry"), ("copilot", "remove the MCP")]),
        ("copilot, then vscode", &[("copilot", ""), ("vscode", "no atticdb entry needed")]),
    ];
    let mcp_config = copilot.join("mcp-config.json");
    for (case, runs) in rows {
        std::fs::remove_dir_all(&copilot).unwrap();
        let _ = std::fs::remove_file(&vscode);
        for (apps, said) in runs {
            let (code, out, err) = run(&["--yes", "--harness", apps]);
            assert_eq!(code, Some(0), "{case} {apps}: {out}{err}");
            // With "", no line names mcp.json.
            let line = format!("\n  {v}: {said}");
            assert_eq!(out.contains(&line), !said.is_empty(), "{case} {apps}: {out}");
        }
        let found = [(&vscode, "servers"), (&mcp_config, "mcpServers")]
            .map(|(file, section)| file.exists() && read(file)[section].get("atticdb").is_some());
        assert_eq!(found, [false, true], "{case}: 1 entry, Copilot CLI's");
    }
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
    // The same, plus an atticdb on PATH that is this one, so hints name it atticdb.
    let found = on_path(&dir.0.join("found"));
    let code_found = std::env::join_paths([&code_only, &found]).unwrap();
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
    let rows: [Row<'_>; 19] = [
        ("claude-code and codex", &[], &terminal, &path, "claude-code,codex\n\nn\ny\n", 0, ""),
        ("declined", &[], &terminal, &path, "codex\nn\n\nno\n", 0, "N] Nothing changed.\n"),
        ("not a terminal", &[], &[], &path, "claude-code\n\n\ny\n", 2, needs),
        ("end of stdin", &[], &terminal, &path, "claude-code\n", 2, "no answer on stdin"),
        ("3 unknown apps", &[], &terminal, &path, "claude\nvs code\ncodex,\n", 2, "after 3 tries"),
        ("no apps", &["--yes"], &[], &empty, "", 1, "atticdb found no apps"),
        ("no apps, atticdb on PATH", &["--yes"], &[], &found, "", 1, "atticdb found no apps"),
        ("own guide", &["--yes", "--harness", "copilot"], &[], &path, "", 0, own),
        ("VS Code only", &["--yes"], &[], &code_only, "", 0, "\n1 connected, 0 with skipped"),
        ("VS Code, atticdb on PATH", &["--yes"], &[], &code_found, "", 0, "\n1 connected, 0 "),
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
        // Only an apply that fully worked offers "Try it".
        assert_eq!(out.contains("Try it:"), wanted == 0 && out.contains("Applying:\n"), "{case}");
        // Without this atticdb on PATH, hints name it by its full path.
        let atticdb = if case.contains("atticdb on PATH") { "atticdb".into() } else { typed_exe() };
        if case.starts_with("no apps") {
            let shell = format!(
                "Install one, or name it with --harness.\nYou can still try memory from the \
                 shell: {atticdb} fact set --key repo/demo/rule --content \"Use pnpm\" and \
                 {atticdb} search --query pnpm\n"
            );
            assert!(out.ends_with(&shell), "{case}: {out}");
        }
        if case == "VS Code, atticdb on PATH" {
            assert!(out.ends_with(&try_it(&dir.0, &atticdb)), "{case}: {out}");
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
            // "Try it" names the repo the hook reads: the folder holding .git, quoted when
            // it has a space.
            let sub = dir.0.join("my repo/sub");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::create_dir(dir.0.join("my repo/.git")).unwrap();
            let (code, out, err) = setup(&home, &sub, path, env, args, stdin);
            assert_eq!(code, Some(0), "{case}: in a repo: {out}{err}");
            let key = format!(
                "Try it: {atticdb} fact set --key \"repo/my repo/rule\" --content \"Use pnpm, \
                 not npm\"\n"
            );
            assert!(out.contains(&key), "{case}: {out}");
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
        let asked = "Which apps? [copilot,vscode,claude-code,codex] Load memory at chat start, \
                     and save Claude Code's compaction summaries? [Y/n] Save notes without asking? (Claude Code and Codex. Saving \
                     a rule always asks.) [Y/n] Plan:\nClaude Code:\n";
        assert!(out.starts_with(asked), "{case}: {out}");
        assert!(out.contains("Apply? [y/N] Applying:\n"), "{case}: {out}");
        assert!(out.contains("\n2 connected, 0 with skipped parts.\n"), "{case}: {out}");
        assert!(out.contains("\n  Claude Code: ok\n  Codex: ok\n"), "{case}: {out}");
        let next = format!(
            "Next:\n  Open Codex once and approve the atticdb hook; Codex runs a new hook only \
             after that.\n  Start new chat sessions to load memory.\n{}",
            try_it(&dir.0, &typed_exe())
        );
        assert!(out.ends_with(&next), "{case}: {out}");
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

/// Doctor compares the version the server's `/healthz` gives with this binary's. A server on
/// this version is ok. One on another version, or too old to send one, fails. Its line says
/// to restart it, and names the pid in `LOCK`.
#[test]
fn doctor_compares_the_server_version() {
    let dir = Dir::new();
    let (home, empty) = (dir.0.join("h"), dir.0.join("empty"));
    for made in [&home, &empty] {
        std::fs::create_dir(made).unwrap();
    }
    let no_apps = OsString::from(&empty);
    let data = data_dir(&home);
    let _holders = Holders(vec![data.clone()]);
    let version = atticdb::VERSION;
    let doctor = || atticdb("doctor", &home, &dir.0, &no_apps, &[], &[], "");
    // A read sets up the home and starts a server from this binary.
    let list = ["list", "--prefix", "x"];
    let (code, out, err) = atticdb("fact", &home, &dir.0, &no_apps, &[], &list, "");
    assert_eq!(code, Some(0), "{out}{err}");
    let (code, out, err) = doctor();
    assert_eq!(code, Some(0), "{out}{err}");
    let ok = format!("\nServer and store: ok\n  server version: {version}: ok\n");
    assert!(out.contains(&ok), "{out}{err}");

    // This process stands in for an older server. It holds the lock and names its socket.
    assert!(support::kill_holder(&data));
    let socket = data.join("old.sock");
    let mut lock = File::create(data.join("LOCK")).unwrap();
    lock.try_lock().unwrap();
    let pid = std::process::id();
    let holder = format!("pid={pid} instance=X unix={} tcp=none\n", socket.display());
    lock.write_all(holder.as_bytes()).unwrap();
    let advice = format!("this binary is {version}: restart it with atticdb upgrade, or stop pid");
    let rows = [
        ("older", json!({"status": "ok", "instance": "X", "version": "0.0.1"}), "0.0.1"),
        ("no version", json!({"status": "ok", "instance": "X"}), "unknown"),
    ];
    for (case, health, said) in rows {
        let _ = std::fs::remove_file(&socket);
        support::fake_store(&socket, Some(Duration::ZERO), health);
        let (code, out, err) = doctor();
        assert_eq!(code, Some(1), "{case}: {out}{err}");
        let line = format!("\nServer and store: ok\n  server version: {said}, {advice} {pid}\n");
        assert!(out.contains(&line), "{case}: {out}{err}");
    }
    drop(lock);
}

/// An AtticDB binary that doesn't exist.
const OLD: &str = "/old/atticdb";
/// What doctor says about a home that isn't set up, with `atticdb` as it names the binary.
fn not_set_up(atticdb: &str) -> String {
    format!("not set up yet. Run {atticdb} setup, or try memory from the shell with {atticdb} fact set\n")
}
/// Codex's line when `codex mcp get atticdb --json` shows this AtticDB.
const GET_OK: &str = "  codex mcp get atticdb: ok\n";

/// A case, `HOME`, `ATTICDB_HOME`, `PATH`, the args, the exit code, and text stdout or stderr
/// shows.
type DoctorRow<'a> =
    (&'a str, &'a Path, Option<&'a Path>, &'a OsString, &'a [&'a str], i32, String);

/// `atticdb doctor` on a home that setup connected to all 4 fake apps, then with 1 thing
/// wrong per row. A failed check, or a home that isn't set up, exits 1. A warning, or an app
/// not found, exits 0. It runs only `codex mcp get`, and writes nothing in `HOME` or the home
/// but the server's own files in `data/`. VS Code counts as connected through either file.
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
    let (shared_hook, version) = (home.join(".copilot/hooks/atticdb.json"), atticdb::VERSION);
    let sandbox = "Sandboxes:\n  Agents in a sandbox need read and write access to";
    let unset = |atticdb: &str| {
        let unset = format!("Home:\n  {n} (ATTICDB_HOME is not set): {}", not_set_up(atticdb));
        format!("{unset}{sandbox} {n}.\n")
    };
    let found = on_path(&dir.0.join("found"));
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
        format!("Claude Code:\n  parts: 2 would change, {}\n", fix("claude-code")),
        format!("    {}: update the SessionStart hook\n", settings.display()),
        format!("    {}: update the PostCompact hook\n", settings.display()),
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
    let moved_unset = |home: &Path| {
        format!("  {} (set by ATTICDB_HOME): {}", home.display(), not_set_up(&typed_exe()))
    };
    let both = ["--harness", "copilot,bogus"];
    let stale_apps = ["--harness", "codex,vscode,claude-code"];
    // Only the PostCompact group runs a binary that is gone.
    let stale_compact = [
        format!("Claude Code:\n  parts: 1 would change, {}\n", fix("claude-code")),
        format!("    {}: update the PostCompact hook\n", settings.display()),
        format!("  binary: stale, {}\n{}  hook: ok\n", fix("claude-code"), gone(&settings)),
    ];
    // No SessionStart group, and a PostCompact group that runs a binary that is gone. The
    // stale group shows, so the missing SessionStart one isn't taken as left out.
    let no_start = [
        format!("Claude Code:\n  parts: 2 would change, {}\n", fix("claude-code")),
        format!("    {}: add the SessionStart hook\n", settings.display()),
        format!("    {}: update the PostCompact hook\n", settings.display()),
        format!("  binary: stale, {}\n{}", fix("claude-code"), gone(&settings)),
    ];
    let claude_only = ["--harness", "claude-code"];
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
    // VS Code is connected through Copilot CLI's entry, or its own in mcp.json. Both list 2.
    let vscode_ok = "\nVS Code:\n  parts: ok\n  binary: ok\n  hook: ok\n".to_owned();
    let both_files = format!(
        "\nVS Code:\n  parts: 1 would change, {}\n    {v}: remove the MCP entry. VS Code 1.144 \
         and later read it from {}\n  binary: ok\n  hook: ok\n",
        fix("vscode"),
        entry.display()
    );
    let vscode_only = ["--harness", "vscode"];
    let rows: [DoctorRow<'_>; 22] = [
        ("not set up", &bare, None, &no_apps, &[], 1, unset(&typed_exe())),
        ("not set up, atticdb on PATH", &bare, None, &found, &[], 1, unset("atticdb")),
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
        ("stale PostCompact hook", &home, None, &path, &claude_only, 1, stale_compact.concat()),
        (
            "stale PostCompact, no SessionStart",
            &home,
            None,
            &path,
            &claude_only,
            1,
            no_start.concat(),
        ),
        ("home 0755", &home, None, &path, &[], 1, mode),
        ("unknown harness", &home, None, &path, &both, 2, "usage:".into()),
        ("synced folder", &bare, moved, &no_apps, &[], 0, moved_ok),
        ("bad config", &bare, Some(&bad), &no_apps, &[], 1, config),
        ("codex bad toml", &home, None, &path, &codex, 1, get_failed.into()),
        ("codex not on PATH", &home, None, &no_codex, &codex, 0, not_verified.into()),
        ("codex hooks off", &home, None, &path, &codex, 0, off),
        ("symlinked binary", &home, None, &path, &copilot, 1, linked),
        ("vscode via copilot's entry", &home, None, &path, &vscode_only, 0, vscode_ok.clone()),
        ("vscode via mcp.json", &home, None, &path, &vscode_only, 0, vscode_ok),
        ("vscode in both files", &home, None, &path, &vscode_only, 1, both_files),
    ];
    // VS Code's own entry, as connect writes it when Copilot CLI has none.
    let vscode_entry = |command: &str| {
        let entry = json!({"type": "stdio", "command": command,
            "args": ["mcp", "--agent", "vscode-copilot"]});
        json!({"servers": {"atticdb": entry}}).to_string()
    };
    let drop_copilot = || {
        let mut config = read(&entry);
        config["mcpServers"].as_object_mut().unwrap().remove("atticdb");
        std::fs::write(&entry, config.to_string()).unwrap();
    };
    let shared = &shared_hook;
    // Setup wrote no mcp.json, as VS Code reads Copilot CLI's entry. Rows may write one.
    std::fs::create_dir_all(vscode.parent().unwrap()).unwrap();
    for (case, row_home, moved, path, args, wanted, said) in rows {
        let files = [&agents, &vscode, shared, &settings, &toml, &entry];
        let saved = files.map(|file| std::fs::read(file).ok());
        match case {
            "part would change" => std::fs::remove_file(&agents).unwrap(),
            // A binary that is gone, and one that exists but isn't this one.
            // VS Code's own entry counts only without Copilot CLI's.
            "stale binary" => {
                std::fs::write(&vscode, vscode_entry(OLD)).unwrap();
                drop_copilot();
                let exe = exe.to_str().unwrap();
                for (file, other) in [(shared, "/bin/sh"), (&settings, OLD), (&toml, OLD)] {
                    let text = std::fs::read_to_string(file).unwrap();
                    std::fs::write(file, text.replace(exe, other)).unwrap();
                }
            }
            "stale PostCompact, no SessionStart" => {
                let mut root = read(&settings);
                root["hooks"].as_object_mut().unwrap().remove("SessionStart");
                let ours = format!("'{}' hook post-compact", exe.display());
                let text = root.to_string();
                assert!(text.contains(&ours), "{text}");
                let old = text.replace(&ours, &format!("'{OLD}' hook post-compact"));
                std::fs::write(&settings, old).unwrap();
            }
            "stale PostCompact hook" => {
                let text = std::fs::read_to_string(&settings).unwrap();
                let ours = format!("'{}' hook post-compact", exe.display());
                assert!(text.contains(&ours), "{text}");
                let old = text.replace(&ours, &format!("'{OLD}' hook post-compact"));
                std::fs::write(&settings, old).unwrap();
            }
            // Valid for AtticDB's scanner, but not TOML, so only codex finds it.
            "codex bad toml" => {
                let saved = saved[4].as_deref().unwrap();
                std::fs::write(&toml, [b"model = ???\n", saved].concat()).unwrap()
            }
            "codex hooks off" => {
                let saved = saved[4].as_deref().unwrap();
                std::fs::write(&toml, [b"[features]\nhooks = false\n\n", saved].concat()).unwrap()
            }
            "symlinked binary" => {
                let mut config = read(&entry);
                config["mcpServers"]["atticdb"]["command"] = json!(link.to_str().unwrap());
                std::fs::write(&entry, config.to_string()).unwrap();
            }
            "home 0755" => support::chmod(&atticdb_home, 0o755),
            "vscode via mcp.json" => {
                std::fs::write(&vscode, vscode_entry(exe.to_str().unwrap())).unwrap();
                drop_copilot();
            }
            "vscode in both files" => {
                std::fs::write(&vscode, vscode_entry(exe.to_str().unwrap())).unwrap()
            }
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
            "not set up" | "not set up, atticdb on PATH" => {
                assert!(tree(&bare).is_empty(), "{case}: no home made")
            }
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
                let server = format!("  server version: {version}: ok");
                let wanted = ["  synced folder: ok", "Server and store: ok", &server];
                assert_eq!(lines[6..9], wanted, "{case}: {out}");
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
            match bytes {
                Some(bytes) => std::fs::write(file, bytes).unwrap(),
                None => {
                    let _ = std::fs::remove_file(file);
                }
            }
        }
    }
    assert_eq!(calls(&claude_log), claude_calls, "claude never ran");
    let gets = &calls(&codex_log)[codex_calls.len()..];
    assert!(gets.iter().all(|args| *args == ["mcp", "get", "atticdb", "--json"]), "{gets:?}");
    let others = dir.0.join("other");
    assert!(!others.join("copilot.argv").exists() && !others.join("code.argv").exists());
}

/// The entry-only apps on this OS: name, name in sentences, config. Claude Desktop has no
/// Linux app.
fn plain_apps(home: &Path) -> Vec<(&'static str, &'static str, PathBuf)> {
    let mut apps =
        vec![("antigravity", "Antigravity", home.join(".gemini/config/mcp_config.json"))];
    if cfg!(target_os = "macos") {
        let config = "Library/Application Support/Claude/claude_desktop_config.json";
        apps.push(("claude-desktop", "Claude Desktop", home.join(config)));
    }
    apps
}

/// `--harness` for [`plain_apps`].
fn plain_list(home: &Path) -> String {
    plain_apps(home).iter().map(|(name, ..)| *name).collect::<Vec<_>>().join(",")
}

/// A `HOME` under `dir` with the dirs of Antigravity and Claude Desktop, and an empty `PATH`.
/// Antigravity's are `~/.gemini/antigravity` and `~/.gemini/config`, with no config file.
fn plain_home(dir: &Path) -> (PathBuf, OsString) {
    let home = dir.join("h");
    for (.., config) in plain_apps(&home) {
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    }
    std::fs::create_dir_all(home.join(".gemini/antigravity")).unwrap();
    std::fs::create_dir(dir.join("empty")).unwrap();
    (home, OsString::from(dir.join("empty")))
}

/// Setup's plan lists each new app whose dir exists, and none that's missing. `~/.gemini`
/// alone doesn't find Gemini CLI. Applying connects Antigravity and Claude Desktop, checks
/// each, and says to restart them. A rerun has nothing to restart.
#[test]
fn setup_detects_new_apps() {
    let dir = Dir::new();
    let (home, empty) = plain_home(&dir.0);
    let _holders = Holders(vec![data_dir(&home)]);
    let (code, out, err) = setup(&home, &dir.0, &empty, &[], &["--dry-run", "--yes"], "");
    for (_, title, config) in plain_apps(&home) {
        let plan = format!("\n{title}:\n  {}: add the MCP entry\n", config.display());
        assert!(out.contains(&plan), "{title} {code:?}: {out}{err}");
    }
    // Antigravity's dirs in `~/.gemini` don't make Gemini CLI found.
    assert!(!out.contains("Gemini CLI:"), "{out}");
    for app in ["Copilot CLI:", "VS Code:", "Claude Code:", "Codex:"] {
        assert!(!out.contains(app), "{app}: {out}");
    }
    assert!(tree(&home).values().all(Option::is_none), "the dry run wrote nothing");

    // Gemini CLI's settings.json finds it.
    let other = dir.0.join("g");
    std::fs::create_dir_all(other.join(".gemini")).unwrap();
    std::fs::write(other.join(".gemini/settings.json"), "{}\n").unwrap();
    let (_, out, err) = setup(&other, &dir.0, &empty, &[], &["--dry-run", "--yes"], "");
    assert!(out.contains("\nGemini CLI:\n") && !out.contains("Antigravity:"), "{out}{err}");

    // Antigravity's config file alone finds it, and so does any `~/.gemini/antigravity*` dir.
    let other = dir.0.join("a");
    let config = other.join(".gemini/config/mcp_config.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "{}\n").unwrap();
    let (_, out, err) = setup(&other, &dir.0, &empty, &[], &["--dry-run", "--yes"], "");
    let plan = format!("\nAntigravity:\n  {}: add the MCP entry\n", config.display());
    assert!(out.contains(&plan) && !out.contains("Gemini CLI:"), "{out}{err}");
    let other = dir.0.join("i");
    std::fs::create_dir_all(other.join(".gemini/antigravity-ide")).unwrap();
    let (_, out, err) = setup(&other, &dir.0, &empty, &[], &["--dry-run", "--yes"], "");
    assert!(out.contains("\nAntigravity:\n"), "{out}{err}");

    // Only Claude Desktop's dir, on macOS. Nothing on Linux.
    let other = dir.0.join("o");
    std::fs::create_dir(&other).unwrap();
    let claude = plain_apps(&other).into_iter().find(|(name, ..)| *name == "claude-desktop");
    if let Some((.., config)) = &claude {
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    }
    let (_, out, err) = setup(&other, &dir.0, &empty, &[], &["--dry-run", "--yes"], "");
    assert!(!out.contains("Antigravity:") && !out.contains("Gemini CLI:"), "{out}{err}");
    assert_eq!(out.contains("\nClaude Desktop:\n"), claude.is_some(), "{out}{err}");

    let args = ["--yes", "--harness", &plain_list(&home)];
    let (code, out, err) = setup(&home, &dir.0, &empty, &[], &args, "");
    assert_eq!(code, Some(0), "{out}{err}");
    for (_, title, _) in plain_apps(&home) {
        assert!(out.contains(&format!("\n  {title}: ok\n")), "{title}: {out}");
        assert!(out.contains(&format!("\n  Restart {title} to load it.\n")), "{title}: {out}");
    }
    let (code, out, err) = setup(&home, &dir.0, &empty, &[], &args, "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(!out.contains("Restart "), "{out}");
}

/// Doctor shows each connected new app's entry as ok, and an entry that runs a binary that
/// is gone as stale.
#[test]
fn doctor_reports_new_apps() {
    let dir = Dir::new();
    let (home, empty) = plain_home(&dir.0);
    let _holders = Holders(vec![data_dir(&home)]);
    let list = plain_list(&home);
    let args = ["--yes", "--harness", &list];
    let (code, out, err) = setup(&home, &dir.0, &empty, &[], &args, "");
    assert_eq!(code, Some(0), "{out}{err}");
    let doctor = ["--harness", &list];
    let (code, out, err) = atticdb("doctor", &home, &dir.0, &empty, &[], &doctor, "");
    assert_eq!(code, Some(0), "{out}{err}");
    for (_, title, _) in plain_apps(&home) {
        let ok = format!("\n{title}:\n  parts: ok\n  binary: ok\n");
        assert!(out.contains(&ok), "{title}: {out}");
    }
    assert!(!out.contains("hook:"), "no hook to check: {out}");
    // Found, so listed without --harness too.
    let (_, out, err) = atticdb("doctor", &home, &dir.0, &empty, &[], &[], "");
    assert!(out.contains("\nAntigravity:\n  parts: ok\n"), "{out}{err}");

    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
    for (.., config) in plain_apps(&home) {
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, text.replace(exe.to_str().unwrap(), OLD)).unwrap();
    }
    let (code, out, err) = atticdb("doctor", &home, &dir.0, &empty, &[], &doctor, "");
    assert_eq!(code, Some(1), "{out}{err}");
    for (name, title, config) in plain_apps(&home) {
        let fix = format!("run atticdb setup --harness {name}, or atticdb connect {name}");
        let file = config.display();
        let stale = format!(
            "\n{title}:\n  parts: 1 would change, {fix}\n    {file}: update the MCP entry\n  \
             binary: stale, {fix}\n    {file} runs {OLD}, which no longer exists\n"
        );
        assert!(out.contains(&stale), "{title}: {out}");
    }
}

/// After an upgrade, doctor without `--harness` stays green: a new app that is found but
/// holds no atticdb entry shows no line. Named in `--harness`, it still shows. Setup still
/// offers it.
#[test]
fn doctor_hides_found_but_unconnected_new_apps() {
    let dir = Dir::new();
    let (home, empty) = plain_home(&dir.0);
    let _holders = Holders(vec![data_dir(&home)]);
    // Connect only Antigravity, which sets up the home. Then disconnect it.
    let args = ["--yes", "--harness", "antigravity"];
    let (code, out, err) = setup(&home, &dir.0, &empty, &[], &args, "");
    assert_eq!(code, Some(0), "{out}{err}");
    let (code, out, err) = atticdb("disconnect", &home, &dir.0, &empty, &[], &["antigravity"], "");
    assert_eq!(code, Some(0), "{out}{err}");
    // Gemini CLI found by its settings.json, with no atticdb entry.
    std::fs::write(home.join(".gemini/settings.json"), "{\"theme\": \"GitHub\"}\n").unwrap();
    let (code, out, err) = atticdb("doctor", &home, &dir.0, &empty, &[], &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    for title in ["Gemini CLI", "Antigravity", "Claude Desktop"] {
        assert!(!out.contains(&format!("\n{title}")), "{title}: {out}");
    }
    let mut named = vec![("gemini", "Gemini CLI"), ("antigravity", "Antigravity")];
    if cfg!(target_os = "macos") {
        named.push(("claude-desktop", "Claude Desktop"));
    }
    for (name, title) in named {
        let (code, out, err) =
            atticdb("doctor", &home, &dir.0, &empty, &[], &["--harness", name], "");
        assert_eq!(code, Some(1), "{name}: {out}{err}");
        assert!(out.contains(&format!("\n{title}:\n  parts: ")), "{name}: {out}");
    }
    let (_, out, err) = setup(&home, &dir.0, &empty, &[], &["--dry-run", "--yes"], "");
    for title in ["Gemini CLI", "Antigravity"] {
        assert!(out.contains(&format!("\n{title}:\n")), "{title}: {out}{err}");
    }
}

/// When Gemini CLI's settings turn its hooks off, or turn off the hook named atticdb, setup
/// and doctor say memory won't load at chat start, as for Codex.
#[test]
fn setup_and_doctor_report_gemini_hooks_off() {
    let dir = Dir::new();
    std::fs::create_dir(dir.0.join("empty")).unwrap();
    let empty = OsString::from(dir.0.join("empty"));
    let cases = [
        ("enabled", r#"{"hooksConfig": {"enabled": false}}"#, "sets hooksConfig.enabled to false"),
        (
            "disabled",
            r#"{"hooksConfig": {"disabled": ["x", "atticdb"]}}"#,
            "lists atticdb in hooksConfig.disabled",
        ),
    ];
    for (row, (case, settings, why)) in cases.into_iter().enumerate() {
        let home = dir.0.join(format!("h{row}"));
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let file = gemini.join("settings.json");
        std::fs::write(&file, format!("{settings}\n")).unwrap();
        let _holders = Holders(vec![data_dir(&home)]);
        let args = ["--yes", "--harness", "gemini"];
        let (code, out, err) = setup(&home, &dir.0, &empty, &[], &args, "");
        assert_eq!(code, Some(0), "{case}: {out}{err}");
        let note = format!("\n  note: {} {why}, so Gemini CLI", file.display());
        assert!(out.contains(&note), "{case}: {out}");
        assert!(out.contains(&format!("\n  {GEMINI_HOOKS_OFF}\n")), "{case}: {out}");
        let doctor = ["--harness", "gemini"];
        let (code, out, err) = atticdb("doctor", &home, &dir.0, &empty, &[], &doctor, "");
        assert_eq!(code, Some(0), "{case}: {out}{err}");
        assert!(
            out.contains(&format!("\n  hook: warning: {GEMINI_DOCTOR_OFF}\n")),
            "{case}: {out}"
        );
        assert!(!out.contains("hook: ok"), "{case}: {out}");
        let (_, out, err) = atticdb("connect", &home, &dir.0, &empty, &[], &["gemini"], "");
        assert!(
            err.contains(&format!("atticdb: {} {why}, so Gemini CLI", file.display())),
            "{case}: {out}{err}"
        );
    }
    // Hooks on, or another hook off: no warning.
    for (row, settings) in
        [r#"{"hooksConfig": {"enabled": true, "disabled": ["x"]}}"#].iter().enumerate()
    {
        let home = dir.0.join(format!("on{row}"));
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::write(home.join(".gemini/settings.json"), settings).unwrap();
        let _holders = Holders(vec![data_dir(&home)]);
        let (code, out, err) =
            setup(&home, &dir.0, &empty, &[], &["--yes", "--harness", "gemini"], "");
        assert_eq!(code, Some(0), "{out}{err}");
        assert!(!out.contains("hooks are off") && !out.contains("hooksConfig"), "{out}");
    }
}

/// Setup's next step when Gemini CLI's settings turn its atticdb hook off.
const GEMINI_HOOKS_OFF: &str = "Gemini CLI: hooks are off (hooksConfig in settings.json), so \
                                memory won't load at chat start. Set enabled to true, and \
                                remove \"atticdb\" from disabled.";

/// Doctor's hook warning when Gemini CLI's settings turn its atticdb hook off.
const GEMINI_DOCTOR_OFF: &str = "Gemini CLI's hooks are off, so memory won't load at chat \
                                 start. In hooksConfig in settings.json, set enabled to true, \
                                 and remove \"atticdb\" from disabled";

/// Setup connects Gemini CLI: its entry, its hook and its guide. Doctor finds them in place
/// and runs the hook. So it does after `--notes ask --no-hook`, with no hook to run.
#[test]
fn setup_and_doctor_connect_gemini() {
    let dir = Dir::new();
    std::fs::create_dir(dir.0.join("empty")).unwrap();
    let empty = OsString::from(dir.0.join("empty"));
    let options: [&[&str]; 2] = [&[], &["--notes", "ask", "--no-hook"]];
    for (row, options) in options.into_iter().enumerate() {
        let home = dir.0.join(format!("h{row}"));
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();
        let _holders = Holders(vec![data_dir(&home)]);
        let args = [&["--yes", "--harness", "gemini"][..], options].concat();
        let (code, out, err) = setup(&home, &dir.0, &empty, &[], &args, "");
        assert_eq!(code, Some(0), "{options:?}: {out}{err}");
        assert!(out.contains("\n  Gemini CLI: ok\n"), "{options:?}: {out}");
        let settings: Value =
            serde_json::from_slice(&std::fs::read(gemini.join("settings.json")).unwrap()).unwrap();
        assert_eq!(
            settings["mcpServers"]["atticdb"]["trust"].as_bool(),
            options.is_empty().then_some(true)
        );
        assert_eq!(settings["hooks"].is_object(), options.is_empty(), "{options:?}");
        assert!(std::fs::read_to_string(gemini.join("GEMINI.md")).unwrap().contains("AtticDB"));
        let doctor = ["--harness", "gemini"];
        let (code, out, err) = atticdb("doctor", &home, &dir.0, &empty, &[], &doctor, "");
        assert_eq!(code, Some(0), "{options:?}: {out}{err}");
        let hook = if options.is_empty() { "  hook: ok\n" } else { "" };
        let ok = format!("\nGemini CLI:\n  parts: ok\n  binary: ok\n{hook}");
        assert!(out.contains(&ok), "{options:?}: {out}");
    }
}
