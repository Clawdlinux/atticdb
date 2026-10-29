use crate::support::{self, chmod, files, Dir, Process};
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::process::Stdio;

/// `connect` adds an owner-only file, a rerun writes nothing, and an older entry is updated.
/// An entry with connection flags is replaced only with `--force`. `disconnect` removes only
/// zerodb's entry. Rewrites keep a backup and the file mode. A file that is not a JSON
/// object, or a symlink, stays as it was. No token reaches any output.
#[test]
fn connect_edits_only_the_zerodb_entry() {
    let dir = Dir::new();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_zerodb")).unwrap();
    let mac = cfg!(target_os = "macos");
    let (config, data) = if mac {
        ("Library/Application Support", "Library/Application Support")
    } else {
        (".config", ".local/share")
    };
    let vscode =
        json!({"type": "stdio", "command": exe, "args": ["mcp", "--agent", "vscode-copilot"]});
    let copilot = json!({"type": "local", "command": exe,
        "args": ["mcp", "--agent", "copilot"], "tools": ["*"]});
    let vscode_file = format!("{config}/Code/User/mcp.json");
    let mut rows = vec![
        ("vscode", None, vscode_file, "servers", vscode.clone()),
        ("copilot", None, ".copilot/mcp-config.json".into(), "mcpServers", copilot.clone()),
        ("copilot", Some("COPILOT_HOME"), "c/mcp-config.json".into(), "mcpServers", copilot),
    ];
    if !mac {
        rows.push((
            "vscode",
            Some("XDG_CONFIG_HOME"),
            "c/Code/User/mcp.json".into(),
            "servers",
            vscode,
        ));
    }
    for (row, (name, var, file, section, entry)) in rows.into_iter().enumerate() {
        let home = dir.0.join(row.to_string());
        let case = format!("{name} {var:?}");
        let (file, mut printed) = (home.join(file), String::new());
        let parent = file.parent().unwrap().to_owned();
        let mut run = |args: &[&str]| {
            let mut command = support::command(env!("CARGO_BIN_EXE_zerodb"));
            command.args(args).current_dir(&dir.0).env("HOME", &home).env_remove("ZERODB_TOKEN");
            command.envs(var.map(|var| (var, home.join("c"))));
            let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
            printed.push_str(&out);
            (status.code(), out)
        };
        let read = || serde_json::from_slice::<Value>(&std::fs::read(&file).unwrap()).unwrap();
        let mode = || std::fs::metadata(&file).unwrap().mode() & 0o777;
        let backups =
            || files(&parent).into_keys().filter(|n| n.contains(".zerodb-backup-")).count();
        // Copilot's hook directory sits next to its config.
        let listed = || files(&parent).into_keys().filter(|name| name != "hooks").count();

        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let first = format!("connected zerodb to {name}: {}", file.display());
        assert_eq!(out.lines().next(), Some(first.as_str()), "{case}");
        assert_eq!(read(), json!({section: {"zerodb": entry}}), "{case}: created");
        let pretty = serde_json::to_string_pretty(&read()).unwrap() + "\n";
        assert_eq!(std::fs::read_to_string(&file).unwrap(), pretty, "{case}: 2-space indent");
        assert_eq!(listed(), 1, "{case}: no backup or temp file");
        assert_eq!(mode(), 0o600, "{case}: a new file is owner-only");
        let parent_mode = std::fs::metadata(&parent).unwrap().mode() & 0o777;
        assert_eq!(parent_mode, 0o700, "{case}: new directories are private");

        let before = (files(&parent), std::fs::metadata(&file).unwrap().modified().unwrap());
        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let first = format!("zerodb is already connected to {name} ({})", file.display());
        assert_eq!(out.lines().next(), Some(first.as_str()), "{case}");
        let after = (files(&parent), std::fs::metadata(&file).unwrap().modified().unwrap());
        assert_eq!(after, before, "{case}: nothing written");

        // An entry with any connection flag keeps its store: refused unless `--force`.
        let force = format!("zerodb connect {name} --force");
        for flag in ["--url", "--data-dir", "--config", "--token-file", "--tenant"] {
            let mut flagged = read();
            flagged[section]["zerodb"]["args"] = json!(["mcp", flag, "/old"]);
            std::fs::write(&file, serde_json::to_vec(&flagged).unwrap()).unwrap();
            let before = files(&parent);
            let (code, out) = run(&["connect", name]);
            assert_eq!(code, Some(1), "{case} {flag}: {out}");
            let used = format!("uses {flag} /old.");
            assert!(out.contains(&used) && out.contains(&force), "{case} {flag}: {out}");
            assert_eq!(files(&parent), before, "{case} {flag}: entry unchanged");
        }
        let (code, out) = run(&["connect", name, "--force"]);
        assert_eq!(code, Some(0), "{case}: {out}");
        assert!(out.starts_with(&format!("updated zerodb in {name}: ")), "{case}: {out}");
        assert_eq!((read()[section]["zerodb"].clone(), backups()), (entry.clone(), 1), "{case}");

        // An older entry, another server and another top-level key, added by hand.
        let mut edited = read();
        edited[section]["zerodb"]["args"] = json!(["mcp"]);
        edited[section]["other"] = json!({"command": "other"});
        edited["editor.fontSize"] = json!(14);
        let edited_bytes = serde_json::to_vec(&edited).unwrap();
        std::fs::write(&file, &edited_bytes).unwrap();
        chmod(&file, 0o600);
        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let prefix = format!("updated zerodb in {name}: {} (backup: ", file.display());
        let first = out.lines().next().unwrap();
        assert!(first.starts_with(&prefix) && first.ends_with(", keys are now sorted)"), "{out}");
        edited[section]["zerodb"] = entry;
        assert_eq!(read(), edited, "{case}: only the entry changed");
        let backup = first[prefix.len()..].split(", ").next().unwrap().to_owned();
        assert_eq!(std::fs::read(&backup).unwrap(), edited_bytes, "{case}: backup");
        let backup_mode = std::fs::metadata(&backup).unwrap().mode() & 0o777;
        assert_eq!(backup_mode, 0o600, "{case}: the backup keeps the mode");
        assert_eq!((backups(), mode()), (2, 0o600), "{case}: backup, mode kept");

        let (code, out) = run(&["disconnect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        assert!(out.starts_with(&format!("disconnected zerodb from {name}: ")), "{case}: {out}");
        edited[section].as_object_mut().unwrap().remove("zerodb");
        assert_eq!(read(), edited, "{case}: the other server and key stay");
        assert_eq!((backups(), mode()), (3, 0o600), "{case}: another backup, mode kept");
        assert_eq!(listed(), 4, "{case}: no temp file left");
        let before = files(&parent);
        let (code, out) = run(&["disconnect", name]);
        assert_eq!((code, out), (Some(0), format!("zerodb is not connected to {name}\n")));
        assert_eq!(files(&parent), before, "{case}: nothing written");

        // Not a JSON object, or a symlink: exit 1 and change nothing.
        std::fs::write(&file, "// my servers\n{}\n").unwrap();
        let before = files(&parent);
        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(1), "{case}: {out}");
        assert!(out.contains(&file.display().to_string()) && out.contains("by hand"), "{out}");
        assert_eq!(files(&parent), before, "{case}: comment file unchanged");
        std::fs::remove_file(&file).unwrap();
        std::fs::write(parent.join("real.json"), "{}").unwrap();
        std::os::unix::fs::symlink("real.json", &file).unwrap();
        let before = files(&parent);
        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(1), "{case}: {out}");
        assert!(out.contains("symlink"), "{case}: {out}");
        assert_eq!(files(&parent), before, "{case}: symlink unchanged");
        assert!(std::fs::symlink_metadata(&file).unwrap().is_symlink(), "{case}");

        // The token stays in the home, and connect starts no server.
        let home_dir = home.join(data).join("zerodb");
        let config: Value =
            serde_json::from_slice(&std::fs::read(home_dir.join("zerodb.json")).unwrap()).unwrap();
        let token = config["tenants"][0]["token"].as_str().unwrap();
        let leaked = files(&parent).into_values().flatten();
        let mut seen = leaked.chain([printed.into_bytes()]);
        let token = token.as_bytes();
        assert!(!seen.any(|bytes| bytes.windows(token.len()).any(|w| w == token)), "{case}: leak");
        assert_eq!(files(&home_dir.join("data")).len(), 0, "{case}: no server started");
    }
    let mut command = support::command(env!("CARGO_BIN_EXE_zerodb"));
    let (status, out) =
        Process::start(command.args(["connect", "emacs"]).env("HOME", &dir.0).stdin(Stdio::null()))
            .wait();
    assert_eq!(status.code(), Some(2), "{out}");
    assert!(out.contains("zerodb connect <vscode|copilot>"), "{out}");
}

/// `connect` writes 1 hook file for both harnesses, and `disconnect` removes it once neither
/// is connected. A hook file zerodb did not write stays. The quoted path runs in `sh`. Under
/// `ZERODB_HOME`, both entries and the hook pass it on, and a path with a `$` is refused.
/// Both entries must pass the same home, as the hook reads 1.
#[test]
fn connect_writes_the_session_start_hook() {
    let dir = Dir::new();
    // A binary path with a quote, which the hook command must escape.
    let bin = dir.0.join("it's");
    std::fs::create_dir(&bin).unwrap();
    let exe = bin.join("zerodb");
    std::fs::copy(env!("CARGO_BIN_EXE_zerodb"), &exe).unwrap();
    let exe = std::fs::canonicalize(exe).unwrap();
    let bash = format!("'{}' hook session-start", exe.to_str().unwrap().replace('\'', r"'\''"));
    let wanted = json!({"version": 1, "hooks": {"sessionStart": [
        {"type": "command", "bash": bash, "timeoutSec": 5}]}});
    let config = if cfg!(target_os = "macos") { "Library/Application Support" } else { ".config" };
    let env_of = |path: &std::path::Path, section: &str| {
        let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        config[section]["zerodb"]["env"].clone()
    };
    let rows = [None, Some("COPILOT_HOME"), Some("ZERODB_HOME")];
    for (row, var) in rows.into_iter().enumerate() {
        let home = dir.0.join(row.to_string());
        let copilot =
            if var == Some("COPILOT_HOME") { home.join("c") } else { home.join(".copilot") };
        let hooks = copilot.join("hooks");
        let file = hooks.join("zerodb.json");
        let moved = var == Some("ZERODB_HOME");
        let env = if moved { json!({"ZERODB_HOME": home.join("c")}) } else { Value::Null };
        let mut wanted = wanted.clone();
        if moved {
            wanted["hooks"]["sessionStart"][0]["env"] = env.clone();
        }
        let run = |args: &[&str]| {
            let mut command = support::command(&exe);
            command.args(args).current_dir(&dir.0).env("HOME", &home).env_remove("ZERODB_TOKEN");
            command.envs(var.map(|var| (var, home.join("c"))));
            let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
            assert_eq!(status.code(), Some(0), "{var:?} {args:?}: {out}");
            out
        };
        let shown = file.display();
        let out = run(&["connect", "vscode"]);
        assert!(out.ends_with(&format!("\nadded the sessionStart hook: {shown}\n")), "{out}");
        let pretty = serde_json::to_string_pretty(&wanted).unwrap() + "\n";
        assert_eq!(std::fs::read_to_string(&file).unwrap(), pretty, "{var:?}");
        assert_eq!(std::fs::metadata(&hooks).unwrap().mode() & 0o777, 0o700, "{var:?}");
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600, "{var:?}");
        let vscode = home.join(config).join("Code/User/mcp.json");
        assert_eq!(env_of(&vscode, "servers"), env, "{var:?}: VS Code's entry");
        let before = (files(&hooks), std::fs::metadata(&file).unwrap().modified().unwrap());
        let out = run(&["connect", "copilot"]);
        assert!(out.ends_with(&format!("\nthe sessionStart hook is already in place ({shown})\n")));
        let after = (files(&hooks), std::fs::metadata(&file).unwrap().modified().unwrap());
        assert_eq!(after, before, "{var:?}: nothing written");
        let entry = env_of(&copilot.join("mcp-config.json"), "mcpServers");
        assert_eq!(entry, env, "{var:?}: Copilot's entry");

        let out = run(&["disconnect", "vscode"]);
        let kept = format!("\nkept the sessionStart hook ({shown}): zerodb is still connected");
        assert!(out.contains(&kept), "{var:?}: {out}");
        assert_eq!(files(&hooks), before.0, "{var:?}: kept");
        let out = run(&["disconnect", "copilot"]);
        assert!(out.ends_with(&format!("\nremoved the sessionStart hook: {shown}\n")), "{out}");
        assert_eq!(files(&hooks).len(), 0, "{var:?}: removed, no backup");

        // The hook for another binary goes. A hook file with other content stays.
        let mut other = wanted.clone();
        other["hooks"]["sessionStart"][0]["bash"] =
            json!(r"'/opt/it'\''s/zerodb' hook session-start");
        let other = serde_json::to_string_pretty(&other).unwrap() + "\n";
        // A config with comments and no zerodb entry does not keep the hook.
        std::fs::write(home.join(config).join("Code/User/mcp.json"), "// mine\n{}\n").unwrap();
        let rows = [
            (other.as_str(), "removed the sessionStart hook", None),
            ("{}", "alone: it differs from the hook zerodb writes", Some("{}")),
        ];
        for (content, said, found) in rows {
            std::fs::write(&file, content).unwrap();
            let out = run(&["disconnect", "copilot"]);
            assert!(out.contains(said), "{var:?} {content}: {out}");
            let left = std::fs::read_to_string(&file).ok();
            assert_eq!(left.as_deref(), found, "{var:?}: {out}");
        }
    }
    // The command runs in `sh` as the harness runs it.
    let mut sh = support::command("sh");
    sh.args(["-c", &bash]).env("HOME", dir.0.join("0")).stdin(Stdio::null());
    let (status, out) = Process::start(&mut sh).wait();
    assert_eq!((status.code(), out.lines().next()), (Some(0), Some("{}")), "{out}");

    // Copilot expands variables in a hook's env, so a home path with a `$` is refused.
    let (home, moved) = (dir.0.join("3"), dir.0.join("$HOME"));
    let mut command = support::command(&exe);
    command.args(["connect", "copilot"]).env("HOME", &home).env("ZERODB_HOME", &moved);
    let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
    assert_eq!(status.code(), Some(1), "{out}");
    assert!(out.contains("holds a $") && out.contains("Nothing changed"), "{out}");
    assert!(!home.exists() && !moved.exists(), "nothing written: {out}");

    // The hook reads 1 home, so the 2 entries must pass the same one. A mismatch either way is
    // refused and changes nothing. --force goes ahead and names the harness on the other home.
    let (home, zh) = (dir.0.join("4"), dir.0.join("4/zh"));
    let (vscode, copilot) = (home.join(config).join("Code/User/mcp.json"), home.join(".copilot"));
    let (entry, hook) = (copilot.join("mcp-config.json"), copilot.join("hooks/zerodb.json"));
    let connect = |args: &[&str], moved: bool| {
        let mut command = support::command(&exe);
        command.args(args).current_dir(&dir.0).env("HOME", &home);
        command.envs(moved.then_some(("ZERODB_HOME", &zh)));
        let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
        (status.code(), out)
    };
    let now = || [&vscode, &entry, &hook].map(|path| std::fs::read(path).ok());
    let hook_env = || {
        let hook: Value = serde_json::from_slice(&std::fs::read(&hook).unwrap()).unwrap();
        hook["hooks"]["sessionStart"][0]["env"].clone()
    };
    let (moved, fix) = (format!("ZERODB_HOME {}", zh.display()), "Nothing changed. Connect");
    assert_eq!(connect(&["connect", "vscode"], false).0, Some(0), "VS Code on the default home");
    let before = now();
    let (code, out) = connect(&["connect", "copilot"], true);
    let said = format!("vscode's zerodb entry uses the default home, but this would use {moved}.");
    assert_eq!(code, Some(1), "mismatch: {out}");
    assert!(out.contains(&said) && out.contains(fix), "mismatch: {out}");
    assert!(out.contains("vscode again under the same ZERODB_HOME, or disconnect"), "{out}");
    assert_eq!(now(), before, "mismatch: no file changed");
    assert!(!zh.exists(), "mismatch: no home made");
    let (code, out) = connect(&["connect", "copilot", "--force"], true);
    let said = format!(
        "vscode still uses the default home, but the shared sessionStart hook now \
        reads {moved}."
    );
    assert_eq!(code, Some(0), "--force: {out}");
    assert!(out.contains(&said), "--force: {out}");
    let env = json!({"ZERODB_HOME": zh});
    assert_eq!((env_of(&entry, "mcpServers"), hook_env()), (env.clone(), env.clone()), "--force");
    // The other way round: VS Code again with ZERODB_HOME unset.
    let before = now();
    let (code, out) = connect(&["connect", "vscode"], false);
    let said = format!("copilot's zerodb entry uses {moved}, but this would use the default home.");
    assert_eq!(code, Some(1), "unset: {out}");
    assert!(out.contains(&said) && out.contains(fix), "unset: {out}");
    assert_eq!(now(), before, "unset: no file changed");
    // Matching homes connect cleanly.
    let (code, out) = connect(&["connect", "vscode"], true);
    assert_eq!(code, Some(0), "matching: {out}");
    assert!(!out.contains("still uses"), "matching: {out}");
    assert_eq!((env_of(&vscode, "servers"), hook_env()), (env.clone(), env), "matching");
    // A hand-made VS Code entry in a file with comments passes no ZERODB_HOME, so it is on the
    // default home, as Copilot is about to be.
    std::fs::write(&vscode, "// mine\n{\"servers\": {\"zerodb\": {\"command\": \"zerodb\"}}}\n")
        .unwrap();
    let (code, out) = connect(&["connect", "copilot"], false);
    assert_eq!(code, Some(0), "comments: {out}");
    assert!(!out.contains("still uses"), "comments: {out}");
    assert_eq!((env_of(&entry, "mcpServers"), hook_env()), (Value::Null, Value::Null), "comments");
}
