use crate::support::{self, data_dir, fake_program, tree, Dir, Holders, Process};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

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

/// Runs `zerodb setup` with `args` in `cwd`, with `HOME` set to `home`, `PATH` to `path`, and
/// `env` added. Writes `stdin`, then closes it. Returns the exit code, stdout and stderr.
fn setup(
    home: &Path,
    cwd: &Path,
    path: &OsString,
    env: &[(&str, &str)],
    args: &[&str],
    stdin: &str,
) -> (Option<i32>, String, String) {
    let mut command = support::command(env!("CARGO_BIN_EXE_zerodb"));
    command.arg("setup").args(args).current_dir(cwd).env("HOME", home).env("PATH", path);
    command.env_remove("ZERODB_TOKEN").env_remove("ZERODB_SETUP_INTERACTIVE");
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
/// answers. A rerun changes nothing and says so. A Claude part that can't be applied is
/// skipped, and the exit code is 1.
#[test]
fn setup_plans_then_connects_every_app_it_finds() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir(&home).unwrap();
    let (path, claude_log, codex_log) = fakes(&dir.0);
    let _holders = Holders(vec![data_dir(&home)]);
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_zerodb")).unwrap();
    let exe = exe.to_str().unwrap().to_owned();
    let run = |args: &[&str]| setup(&home, &dir.0, &path, &[], args, "");
    let (copilot, vscode) = (home.join(".copilot"), vscode_file(&home));
    let (state, settings) = (home.join(".claude.json"), home.join(".claude/settings.json"));
    let (config, hooks) = (home.join(".codex/config.toml"), home.join(".codex/hooks.json"));
    let (c, v, h) = (copilot.display(), vscode.display(), home.display());

    let (code, out, err) = run(&["--dry-run", "--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let plan = format!(
        "Plan:\nCopilot CLI:\n  {c}/mcp-config.json: add the MCP entry\n  \
         {c}/hooks/zerodb.json: add the sessionStart hook\n  \
         {c}/instructions/zerodb.instructions.md: add the zerodb instructions\n\
         VS Code:\n  {v}: add the MCP entry\n  VS Code's Copilot chat reads the Copilot hook and \
         instructions in {c}, so VS Code gets those too.\n\
         Claude Code:\n  {h}/.claude.json: add the MCP entry\n  \
         {h}/.claude/settings.json: add the SessionStart hook\n  \
         {h}/.claude/settings.json: add the allow rules\n\
         Codex:\n  {h}/.codex/config.toml: add the MCP entry\n  \
         {h}/.codex/hooks.json: add the SessionStart hook\n\
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
    assert_eq!(lines[1], format!("  connected zerodb to copilot: {c}/mcp-config.json"));
    assert!(applying.contains(&format!("\nClaude Code:\n  MCP entry: added ({h}/.claude.json)\n")));
    assert!(applying.contains(&format!("\nCodex:\n  MCP entry: added ({h}/.codex/config.toml)\n")));
    assert!(applying.contains("\n4 connected, 0 with skipped parts.\nChecks:\n"), "{out}");
    for app in ["Copilot CLI", "VS Code", "Claude Code", "Codex"] {
        assert!(applying.contains(&format!("\n  {app}: ok\n")), "{app}: {out}");
    }
    let next = "Next:\n  Open Codex once and approve the zerodb hook; Codex runs a new hook only \
                after that.\n  Start new chat sessions to load memory.\n  VS Code: reload the \
                window to load zerodb.\n";
    assert!(out.ends_with(next), "{out}");
    assert_eq!(read(&vscode)["servers"]["zerodb"]["command"], json!(exe));
    assert_eq!(
        read(&copilot.join("mcp-config.json"))["mcpServers"]["zerodb"]["command"],
        json!(exe)
    );
    assert!(copilot.join("hooks/zerodb.json").is_file());
    assert!(copilot.join("instructions/zerodb.instructions.md").is_file());
    assert_eq!(read(&state)["mcpServers"]["zerodb"]["command"], json!(exe));
    let allow = read(&settings)["permissions"]["allow"].clone();
    assert!(allow.as_array().unwrap().contains(&json!("mcp__zerodb__memory_append")));
    assert_eq!(read(&settings)["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    let block = std::fs::read_to_string(&config).unwrap();
    assert!(block.contains("[mcp_servers.zerodb.tools.memory_append]"), "{block}");
    assert_eq!(read(&hooks)["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    let add = ["mcp", "add", "-s", "user", "zerodb", "--", &exe, "mcp", "--agent", "claude-code"];
    assert_eq!(calls(&claude_log), [add.map(String::from).to_vec()]);
    // Once after the write, once in the checks. Never a command that changes anything.
    let get = ["mcp", "get", "zerodb", "--json"].map(String::from).to_vec();
    assert_eq!(calls(&codex_log), [get.clone(), get.clone()]);

    let files = [&vscode, &state, &settings, &config, &hooks];
    let before = (files.map(|file| std::fs::read(file).unwrap()), tree(&copilot));
    let (code, out, err) = run(&["--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let plan = out.split("Applying:\n").next().unwrap();
    assert_eq!(plan.lines().filter(|line| line.ends_with(" unchanged")).count(), 9, "{out}");
    for line in out.lines() {
        let changed = [": add the ", ": update the ", "added", "updated", "connected zerodb to"];
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

/// The questions take piped answers under `ZERODB_SETUP_INTERACTIVE=1`, and their defaults
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
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_zerodb")).unwrap();
    let exe = exe.to_str().unwrap().to_owned();
    let terminal = [("ZERODB_SETUP_INTERACTIVE", "1")];
    let needs = "zerodb setup needs a terminal, or --yes. See zerodb setup --help.";
    let own = "keep the user's instructions, which mention zerodb";
    let config_dir = [("CLAUDE_CONFIG_DIR", "rel")];
    let codex = ["--yes", "--harness", "codex"];
    let rows: [Row<'_>; 12] = [
        ("claude-code and codex", &[], &terminal, &path, "claude-code,codex\n\nn\ny\n", 0, ""),
        ("declined", &[], &terminal, &path, "codex\nn\n\nno\n", 0, "N] Nothing changed.\n"),
        ("not a terminal", &[], &[], &path, "claude-code\n\n\ny\n", 2, needs),
        ("end of stdin", &[], &terminal, &path, "claude-code\n", 2, "no answer on stdin"),
        ("3 unknown apps", &[], &terminal, &path, "claude\nvs code\ncodex,\n", 2, "after 3 tries"),
        ("no apps", &["--yes"], &[], &empty, "", 1, "zerodb found no apps"),
        ("own guide", &["--yes", "--harness", "copilot"], &[], &path, "", 0, own),
        ("VS Code only", &["--yes"], &[], &code_only, "", 0, "\n1 connected, 0 with skipped"),
        // Claude Code stops with exit 2 first. Codex then connects, and 2 stays the code.
        ("2 wins", &["--yes", "--harness", "claude-code,codex"], &config_dir, &path, "", 2, ""),
        // The MCP entry is in place, but not the hook, so there is nothing to approve.
        ("Codex hook skipped", &codex, &[], &path, "", 1, "\n  SessionStart hook: skipped, "),
        ("Codex hooks off", &codex, &[], &path, "", 0, "\n  Codex: ok\n"),
        ("help", &["--yes", "--help"], &[], &path, "", 0, "\n  --dry-run         print the plan"),
    ];
    for (row, (case, args, env, path, stdin, wanted, said)) in rows.into_iter().enumerate() {
        let home = dir.0.join(format!("h{row}"));
        std::fs::create_dir(&home).unwrap();
        let _holders = Holders(vec![data_dir(&home)]);
        // The user wrote their own guide, so zerodb's isn't needed.
        let guides = home.join(".copilot/instructions");
        let guide = "Save what you learn with zerodb's memory_append.\n";
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
        let (code, out, err) = setup(&home, &dir.0, path, env, args, stdin);
        assert_eq!(code, Some(wanted), "{case}: {out}{err}");
        assert!(out.contains(said) || err.contains(said), "{case}: {out}{err}");
        if case == "own guide" {
            let (plan, applying) = out.split_once("Applying:\n").unwrap();
            let user = guides.join("x.instructions.md");
            let user = user.display();
            assert!(plan.contains(&format!("\n  {user}: {own}\n")), "{case}: {out}");
            let kept =
                format!("\n  skipped the zerodb instructions: {user} already mentions zerodb\n");
            assert!(applying.contains(&kept), "{case}: {out}");
            assert!(out.contains("\n1 connected, 0 with skipped parts.\n"), "{case}: {out}");
            assert!(out.contains("\n  Copilot CLI: ok\n"), "{case}: {out}");
            assert_eq!(std::fs::read_to_string(guides.join("x.instructions.md")).unwrap(), guide);
            assert!(!guides.join("zerodb.instructions.md").exists(), "{case}");
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
            assert!(home.join(".copilot/hooks/zerodb.json").is_file(), "{case}");
            let (code, out, err) = setup(&home, &dir.0, path, env, args, stdin);
            assert_eq!(code, Some(0), "{case}: again: {out}{err}");
            assert!(out.starts_with("Plan:\nVS Code:\n"), "{case}: {out}");
            assert!(!out.contains("Copilot CLI"), "{case}: {out}");
            assert!(!home.join(".copilot/mcp-config.json").exists(), "{case}");
            continue;
        }
        if case == "help" {
            assert!(out.starts_with("usage: zerodb setup [--yes] [--harness <list>]"), "{out}");
            assert!(err.is_empty(), "{case}: {err}");
        }
        if row > 0 {
            assert!(tree(&home).is_empty(), "{case}: nothing written");
            continue;
        }
        let asked = "Which apps? [copilot,vscode,claude-code,codex] Load memory at the start of \
                     each chat? [Y/n] Save notes without asking? (Claude Code and Codex. Saving \
                     a rule always asks.) [Y/n] Plan:\nClaude Code:\n";
        assert!(out.starts_with(asked), "{case}: {out}");
        assert!(out.contains("Apply? [y/N] Applying:\n"), "{case}: {out}");
        assert!(out.contains("\n2 connected, 0 with skipped parts.\n"), "{case}: {out}");
        assert!(out.contains("\n  Claude Code: ok\n  Codex: ok\n"), "{case}: {out}");
        let next = "Next:\n  Open Codex once and approve the zerodb hook; Codex runs a new hook \
                    only after that.\n  Start new chat sessions to load memory.\n";
        assert!(out.ends_with(next), "{case}: {out}");
        let settings = read(&home.join(".claude/settings.json"));
        let allow = settings["permissions"]["allow"].as_array().unwrap().clone();
        assert_eq!(allow.len(), 4, "{case}: reads only, notes ask: {allow:?}");
        assert!(!allow.contains(&json!("mcp__zerodb__memory_append")), "{case}");
        assert_eq!(settings["hooks"]["SessionStart"].as_array().unwrap().len(), 1, "{case}");
        assert_eq!(read(&home.join(".claude.json"))["mcpServers"]["zerodb"]["command"], json!(exe));
        let block = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(block.contains("[mcp_servers.zerodb]"), "{case}: {block}");
        assert!(!block.contains("memory_append"), "{case}: notes ask: {block}");
        assert!(home.join(".codex/hooks.json").is_file(), "{case}");
        assert!(!home.join(".copilot").exists() && !vscode_file(&home).exists(), "{case}");
    }
    let adds = calls(&claude_log).into_iter().filter(|args| args[1] != "get").count();
    assert_eq!(adds, 1, "claude ran only to add the first row's entry");
    assert!(calls(&codex_log).iter().all(|args| args[..2] == ["mcp", "get"]), "codex only read");
}
