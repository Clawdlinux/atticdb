use crate::support::{self, chmod, files, mkfifo, Dir, Process};
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
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
        // Copilot's hook and instructions directories sit next to its config.
        let shared = ["hooks", "instructions"];
        let listed = || files(&parent).into_keys().filter(|n| !shared.contains(&&**n)).count();

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

/// `connect` writes 1 hook file and 1 instructions file for both harnesses, and `disconnect`
/// removes them once neither is connected. A hook file zerodb did not write stays, and so
/// does an instructions file without the marker line, which connect doesn't overwrite, or an
/// edited one. A symlinked instructions file is left alone and doesn't stop the hook. Another instructions file that
/// mentions zerodb stops ours. The quoted path runs in `sh`. Under `ZERODB_HOME`, both
/// entries and the hook pass it on, and a path with a `$` is refused. Both entries must pass
/// the same home, as the hook reads 1.
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
        let instructions = copilot.join("instructions");
        let guide = instructions.join("zerodb.instructions.md");
        let said_guide =
            |said: &str| format!("\n{said} the zerodb instructions: {}\n", guide.display());
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
        assert!(out.contains(&format!("\nadded the sessionStart hook: {shown}\n")), "{out}");
        assert!(out.ends_with(&said_guide("added")), "{var:?}: {out}");
        let text = format!(
            "---\napplyTo: \"**\"\ndescription: \"How to use zerodb, this user's long-term \
             memory\"\n---\n<!-- written by zerodb connect -->\n\n{}\n",
            zerodb::mcp::INSTRUCTIONS
        );
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), text, "{var:?}");
        assert_eq!(std::fs::metadata(&guide).unwrap().mode() & 0o777, 0o600, "{var:?}");
        let pretty = serde_json::to_string_pretty(&wanted).unwrap() + "\n";
        assert_eq!(std::fs::read_to_string(&file).unwrap(), pretty, "{var:?}");
        assert_eq!(std::fs::metadata(&hooks).unwrap().mode() & 0o777, 0o700, "{var:?}");
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600, "{var:?}");
        let vscode = home.join(config).join("Code/User/mcp.json");
        assert_eq!(env_of(&vscode, "servers"), env, "{var:?}: VS Code's entry");
        let before = (files(&hooks), std::fs::metadata(&file).unwrap().modified().unwrap());
        let out = run(&["connect", "copilot"]);
        assert!(out.contains(&format!("\nthe sessionStart hook is already in place ({shown})\n")));
        let after = (files(&hooks), std::fs::metadata(&file).unwrap().modified().unwrap());
        assert_eq!(after, before, "{var:?}: nothing written");
        let in_place =
            format!("\nthe zerodb instructions are already in place ({})\n", guide.display());
        assert!(out.contains(&in_place), "{var:?}: {out}");
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), text, "{var:?}: kept");
        let entry = env_of(&copilot.join("mcp-config.json"), "mcpServers");
        assert_eq!(entry, env, "{var:?}: Copilot's entry");

        let out = run(&["disconnect", "vscode"]);
        let kept = format!("\nkept the sessionStart hook ({shown}): zerodb is still connected");
        assert!(out.contains(&kept), "{var:?}: {out}");
        assert_eq!(files(&hooks), before.0, "{var:?}: kept");
        let kept = format!("\nkept the zerodb instructions ({}): zerodb is still", guide.display());
        assert!(out.contains(&kept) && guide.exists(), "{var:?}: {out}");
        let out = run(&["disconnect", "copilot"]);
        assert!(out.ends_with(&format!("\nremoved the sessionStart hook: {shown}\n")), "{out}");
        assert_eq!(files(&hooks).len(), 0, "{var:?}: removed, no backup");
        assert!(out.contains(&said_guide("removed")), "{var:?}: {out}");
        assert_eq!(files(&instructions).len(), 0, "{var:?}: instructions removed, no backup");

        // Without the marker line, the instructions file stays, and connect doesn't overwrite it.
        // Another instructions file that mentions zerodb, in any case, stops connect from
        // writing ours. Its backup doesn't.
        std::fs::write(&guide, "mine\n").unwrap();
        let out = run(&["disconnect", "copilot"]);
        let alone = format!("left {} alone: it has no line <!-- written by", guide.display());
        assert!(out.contains(&alone), "{var:?}: {out}");
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), "mine\n", "{var:?}: no marker");
        let out = run(&["connect", "copilot"]);
        let skipped = format!("skipped the zerodb instructions: {} has no line", guide.display());
        assert!(out.contains(&skipped), "{var:?}: {out}");
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), "mine\n", "{var:?}: not written");
        // A symlink is left alone, and the hook is still written.
        std::fs::remove_file(&guide).unwrap();
        std::os::unix::fs::symlink("mine.md", &guide).unwrap();
        run(&["disconnect", "copilot"]);
        assert!(!file.exists(), "{var:?}: no hook");
        let out = run(&["connect", "copilot"]);
        let alone = format!("\nleft {} alone: is a symlink\n", guide.display());
        assert!(out.contains(&alone) && file.exists(), "{var:?}: {out}");
        assert!(std::fs::symlink_metadata(&guide).unwrap().is_symlink(), "{var:?}");
        std::fs::remove_file(&guide).unwrap();
        let other = instructions.join("zerodb-memory.instructions.md");
        let backup = instructions.join("zerodb-memory.instructions.md.bak-1");
        std::fs::write(&other, "Save notes in ZeroDB.\n").unwrap();
        std::fs::write(&backup, "zerodb").unwrap();
        // A FIFO is not read, so it can't block connect.
        let fifo = instructions.join("x.instructions.md");
        mkfifo(&fifo);
        let out = run(&["connect", "copilot"]);
        let skipped = format!("skipped the zerodb instructions: {} already", other.display());
        assert!(out.contains(&skipped) && !guide.exists(), "{var:?}: {out}");
        std::fs::remove_file(&other).unwrap();
        let out = run(&["connect", "copilot"]);
        assert!(out.contains(&said_guide("added")), "{var:?}: the backup doesn't count: {out}");
        // An edited file keeps the marker, but disconnect leaves it, as the edits are the user's.
        std::fs::write(&guide, text.clone() + "my edit\n").unwrap();
        let out = run(&["disconnect", "copilot"]);
        let alone = format!("left {} alone: it differs from the instructions", guide.display());
        assert!(out.contains(&alone), "{var:?}: {out}");
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), text.clone() + "my edit\n");
        std::fs::remove_file(&guide).unwrap();
        std::fs::remove_file(&backup).unwrap();
        std::fs::remove_file(&fifo).unwrap();

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
    // A hand-made VS Code entry in a file with comments passes no ZERODB_HOME, and an empty
    // one counts as unset. Both are on the default home, as Copilot is about to be.
    let rows = [
        ("comments", "// mine\n{\"servers\": {\"zerodb\": {\"command\": \"zerodb\"}}}\n"),
        ("empty", "{\"servers\": {\"zerodb\": {\"command\": \"zerodb\", \"env\": {\"ZERODB_HOME\": \"\"}}}}"),
    ];
    for (case, text) in rows {
        std::fs::write(&vscode, text).unwrap();
        let (code, out) = connect(&["connect", "copilot"], false);
        assert_eq!(code, Some(0), "{case}: {out}");
        assert!(!out.contains("still uses"), "{case}: {out}");
        assert_eq!(
            (env_of(&entry, "mcpServers"), hook_env()),
            (Value::Null, Value::Null),
            "{case}"
        );
    }
    // Copilot's own entry with --data-dir, under a home that also splits from VS Code's. The
    // flags refusal shows first, so --force is never advised past a warning not yet shown.
    let mut flagged: Value = serde_json::from_slice(&std::fs::read(&entry).unwrap()).unwrap();
    flagged["mcpServers"]["zerodb"]["args"] = json!(["mcp", "--data-dir", "/old"]);
    std::fs::write(&entry, serde_json::to_vec(&flagged).unwrap()).unwrap();
    let before = now();
    let (code, out) = connect(&["connect", "copilot"], true);
    assert_eq!(code, Some(1), "flags and split: {out}");
    assert!(out.contains("uses --data-dir /old."), "flags and split: {out}");
    assert!(!out.contains("sessionStart hook"), "flags and split: {out}");
    assert_eq!(now(), before, "flags and split: no file changed");
    // With HOME unset, VS Code's config can't be located. It may have been connected from a
    // shell with HOME set, so its home is unknown: refused, and --force goes ahead and warns.
    let (copilot, zh) = (dir.0.join("5/c"), dir.0.join("5/zh"));
    let unset = |force: bool| {
        let mut command = support::command(&exe);
        command.args(["connect", "copilot"]).current_dir(&dir.0).env_remove("HOME");
        command.args(force.then_some("--force"));
        command.env("COPILOT_HOME", &copilot).env("ZERODB_HOME", &zh);
        let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
        (status.code(), out)
    };
    let said = "vscode's config can't be located (HOME must be set to an absolute path)";
    let (code, out) = unset(false);
    assert_eq!(code, Some(1), "HOME unset: {out}");
    assert!(out.contains(said) && out.contains("Set HOME, or add --force"), "HOME unset: {out}");
    assert!(!copilot.exists() && !zh.exists(), "HOME unset: nothing written: {out}");
    let (code, out) = unset(true);
    assert_eq!(code, Some(0), "HOME unset --force: {out}");
    assert!(out.contains(&format!("zerodb: {said}")), "HOME unset --force: {out}");
    let env = json!({"ZERODB_HOME": zh});
    let hook: Value =
        serde_json::from_slice(&std::fs::read(copilot.join("hooks/zerodb.json")).unwrap()).unwrap();
    let entry = env_of(&copilot.join("mcp-config.json"), "mcpServers");
    assert_eq!((entry, hook["hooks"]["sessionStart"][0]["env"].clone()), (env.clone(), env));
}

/// A settings.json as people keep it: 3 SessionStart groups, 12 allow rules, other keys.
const SETTINGS: &str = r#"{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "model": "opus",
  "env": {
    "DISABLE_TELEMETRY": "1"
  },
  "permissions": {
    "allow": [
      "Bash(git status)",
      "Bash(git diff:*)",
      "Bash(git log:*)",
      "Bash(cargo build:*)",
      "Bash(cargo test:*)",
      "Bash(npm run lint)",
      "Read(~/.zshrc)",
      "WebFetch(domain:docs.rs)",
      "WebSearch",
      "mcp__github__get_issue",
      "mcp__github__list_issues",
      "Edit(src/**)"
    ],
    "deny": [
      "Read(./.env)"
    ],
    "defaultMode": "acceptEdits"
  },
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [
          {
            "type": "command",
            "command": "echo started >> ~/.claude/log"
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "$HOME/bin/load-context.sh",
            "timeout": 10
          }
        ]
      },
      {
        "matcher": "compact",
        "hooks": [
          {
            "type": "command",
            "command": "cat NOTES.md"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "cargo fmt"
          }
        ]
      }
    ]
  },
  "statusLine": {
    "type": "command",
    "command": "~/.claude/statusline.sh"
  }
}
"#;

/// `connect claude-code` adds the MCP entry through `claude mcp add`, and appends the
/// SessionStart hook and the allow rules to `settings.json`, in its config dir. A rerun
/// writes nothing. `disconnect` removes them, leaving a hand-kept file byte for byte as it
/// was. `--notes ask` leaves out `memory_append`, and `--no-hook` the hook. A part that
/// can't be applied is skipped with what to add by hand, the others still apply, and the
/// exit code is 1. A different entry is refused unless `--force`. `claude mcp get` never runs.
#[test]
fn connect_claude_code_adds_its_parts() {
    let dir = Dir::new();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_zerodb")).unwrap();
    let exe = exe.to_str().unwrap().to_owned();
    let (bin, empty) = (dir.0.join("bin"), dir.0.join("empty"));
    let log = support::fake_claude(&bin);
    std::fs::create_dir(&empty).unwrap();
    let calls = || -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    };
    let add = |home: Option<&str>| {
        let mut args = vec!["mcp", "add", "-s", "user"];
        let env = home.map(|home| format!("ZERODB_HOME={home}"));
        args.extend(env.iter().flat_map(|env| ["-e", env.as_str()]));
        args.extend(["zerodb", "--", &exe, "mcp", "--agent", "claude-code"]);
        args.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let remove = ["mcp", "remove", "-s", "user", "zerodb"].map(String::from).to_vec();
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    let command = format!("{quoted} hook session-start --harness claude-code");
    let group = json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]});
    let reads =
        ["mcp__zerodb__memory_search", "mcp__zerodb__memory_recent", "mcp__zerodb__fact_get"];
    let reads = [&reads[..], &["mcp__zerodb__fact_list"]].concat();
    let append = "mcp__zerodb__memory_append";
    let run =
        |home: &Path, config: Option<&Path>, path: &Path, env: &[(&str, &Path)], args: &[&str]| {
            let mut command = support::command(&exe);
            command.args(args).current_dir(&dir.0).env("HOME", home).env("PATH", path);
            command
                .env_remove("ZERODB_TOKEN")
                .envs(config.map(|config| ("CLAUDE_CONFIG_DIR", config)));
            command.envs(env.iter().copied());
            let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
            (status.code(), out)
        };
    let read =
        |path: &Path| serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap();
    let push = |value: &mut Value, path: [&str; 2], items: &[Value]| {
        let slot = &mut value[path[0]][path[1]];
        if slot.is_null() {
            *slot = json!([]);
        }
        slot.as_array_mut().unwrap().extend_from_slice(items);
    };
    let strings = |items: &[&str]| items.iter().map(|item| json!(item)).collect::<Vec<_>>();
    let entry = |home: Option<&str>| {
        let env = home.map_or(json!({}), |home| json!({"ZERODB_HOME": home}));
        let args = ["mcp", "--agent", "claude-code"];
        json!({"type": "stdio", "command": exe, "args": args, "env": env})
    };
    let next = "Start a new Claude Code session to load zerodb.";

    let rows = [
        ("fresh", false, None),
        ("CLAUDE_CONFIG_DIR", true, None),
        ("hand-kept", true, Some(SETTINGS)),
    ];
    for (row, (case, moved, kept)) in rows.into_iter().enumerate() {
        let home = dir.0.join(format!("h{row}"));
        let config_dir = if moved { home.join("c") } else { home.join(".claude") };
        let settings = config_dir.join("settings.json");
        let state = if moved { config_dir.join(".claude.json") } else { home.join(".claude.json") };
        if let Some(text) = kept {
            std::fs::create_dir_all(&config_dir).unwrap();
            std::fs::write(&settings, text).unwrap();
            chmod(&settings, 0o644);
        }
        let config = moved.then_some(config_dir.as_path());
        let connect = |args: &[&str]| {
            run(&home, config, &bin, &[], &[&["connect", "claude-code"], args].concat())
        };
        let disconnect = || run(&home, config, &bin, &[], &["disconnect", "claude-code"]);
        let original: Value = kept.map_or(json!({}), |text| serde_json::from_str(text).unwrap());
        let (shown, at) = (settings.display().to_string(), state.display());
        let start = calls().len();

        let (code, out) = connect(&[]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], format!("MCP entry: added ({at})"), "{case}");
        let backup = kept.is_some().then(|| {
            let backup = lines[1].split(", backup: ").nth(1).unwrap().trim_end_matches(')');
            assert_eq!(std::fs::read_to_string(backup).unwrap(), SETTINGS, "{case}: backup");
            format!(", backup: {backup}")
        });
        let backup = backup.unwrap_or_default();
        assert_eq!(lines[1], format!("SessionStart hook: added ({shown}{backup})"), "{case}");
        assert_eq!(lines[2], format!("allow rules: added ({shown}{backup})"), "{case}");
        assert_eq!(lines[3..], [next], "{case}");
        assert_eq!(calls()[start..], [add(None)], "{case}");
        assert_eq!(read(&state)["mcpServers"]["zerodb"], entry(None), "{case}");
        let mut wanted = original.clone();
        push(&mut wanted, ["hooks", "SessionStart"], std::slice::from_ref(&group));
        push(&mut wanted, ["permissions", "allow"], &strings(&[&reads[..], &[append]].concat()));
        assert_eq!(read(&settings), wanted, "{case}");
        let mode = std::fs::metadata(&settings).unwrap().mode() & 0o777;
        assert_eq!(mode, if kept.is_some() { 0o644 } else { 0o600 }, "{case}: mode");
        if kept.is_none() {
            let pretty = serde_json::to_string_pretty(&wanted).unwrap() + "\n";
            assert_eq!(std::fs::read_to_string(&settings).unwrap(), pretty, "{case}");
            // Under CLAUDE_CONFIG_DIR, the fake claude made the dir first.
            let dir_mode = std::fs::metadata(&config_dir).unwrap().mode() & 0o777;
            assert!(moved || dir_mode == 0o700, "{case}: a new config dir is private");
        }

        let now = || (files(&config_dir), std::fs::read(&state).unwrap(), calls().len());
        let before = now();
        let (code, out) = connect(&[]);
        let unchanged = format!(
            "MCP entry: unchanged ({at})\nSessionStart hook: unchanged ({shown})\n\
             allow rules: unchanged ({shown})\n{next}\n"
        );
        assert_eq!((code, out), (Some(0), unchanged), "{case}");
        assert_eq!(now(), before, "{case}: nothing written, no backup, no claude");

        let (code, out) = connect(&["--notes", "ask"]);
        assert_eq!(code, Some(0), "{case}: {out}");
        assert!(out.contains("\nSessionStart hook: unchanged ("), "{case}: {out}");
        assert!(out.contains("\nallow rules: updated ("), "{case}: {out}");
        let allow = read(&settings)["permissions"]["allow"].clone();
        assert!(!allow.as_array().unwrap().contains(&json!(append)), "{case}: {allow}");

        let (code, out) = disconnect();
        assert_eq!(code, Some(0), "{case}: {out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], format!("MCP entry: removed ({at})"), "{case}");
        assert!(lines[1].starts_with(&format!("SessionStart hook: removed ({shown}, backup: ")));
        assert!(lines[2].starts_with(&format!("allow rules: removed ({shown}, backup: ")));
        assert_eq!(lines.len(), 3, "{case}: {out}");
        assert_eq!(calls().last(), Some(&remove), "{case}");
        assert_eq!(read(&state)["mcpServers"], json!({}), "{case}");
        let emptied = json!({"hooks": {"SessionStart": []}, "permissions": {"allow": []}});
        let check = |when: &str| match kept {
            Some(text) => {
                assert_eq!(std::fs::read_to_string(&settings).unwrap(), text, "{case} {when}");
            }
            None => assert_eq!(read(&settings), emptied, "{case} {when}"),
        };
        check("disconnect");

        let (code, out) = connect(&["--no-hook", "--notes", "ask"]);
        assert_eq!(code, Some(0), "{case}: {out}");
        assert!(!out.contains("SessionStart hook"), "{case}: {out}");
        let mut wanted = if kept.is_some() { original.clone() } else { emptied.clone() };
        push(&mut wanted, ["permissions", "allow"], &strings(&reads));
        assert_eq!(read(&settings), wanted, "{case}: --no-hook --notes ask");
        assert_eq!(disconnect().0, Some(0), "{case}");
        check("--no-hook disconnect");
    }

    // A file jsonedit refuses, or a symlink: the settings parts are skipped with what to add by
    // hand, and the file stays. The MCP entry still goes in. Exit 1.
    let home = dir.0.join("s");
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let rows = [("comment", "invalid JSON at byte 0"), ("symlink", "is a symlink")];
    for (case, said) in rows {
        if case == "symlink" {
            std::fs::remove_file(&settings).unwrap();
            std::fs::write(home.join(".claude/real.json"), "{}").unwrap();
            std::os::unix::fs::symlink("real.json", &settings).unwrap();
        } else {
            std::fs::write(&settings, "// mine\n{}\n").unwrap();
        }
        let before = files(&home.join(".claude"));
        let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
        assert_eq!(code, Some(1), "{case}: {out}");
        assert!(out.starts_with("MCP entry: "), "{case}: {out}");
        let hook = format!("\nSessionStart hook: skipped, {}", settings.display());
        let by_hand = format!("Add this group to hooks.SessionStart by hand: {group}\n");
        assert!(
            out.contains(&hook) && out.contains(said) && out.contains(&by_hand),
            "{case}: {out}"
        );
        let rules = json!([&reads[..], &[append]].concat());
        let by_hand = format!("Add these to permissions.allow by hand: {rules}\n{next}\n");
        assert!(
            out.contains("\nallow rules: skipped, ") && out.ends_with(&by_hand),
            "{case}: {out}"
        );
        assert_eq!(files(&home.join(".claude")), before, "{case}: unchanged");
    }
    assert!(std::fs::symlink_metadata(&settings).unwrap().is_symlink());
    assert_eq!(read(&home.join(".claude.json"))["mcpServers"]["zerodb"], entry(None));

    // Without claude on PATH, the MCP part is skipped with the command to run, and the
    // settings parts still go in.
    let home = dir.0.join("m");
    let start = calls().len();
    let (code, out) = run(&home, None, &empty, &[], &["connect", "claude-code"]);
    assert_eq!(code, Some(1), "{out}");
    let skipped =
        "MCP entry: skipped, claude is not on PATH. Run: claude mcp add -s user zerodb -- ";
    assert!(out.starts_with(skipped), "{out}");
    assert!(out.lines().next().unwrap().ends_with(" mcp --agent claude-code"), "{out}");
    assert!(out.contains("\nSessionStart hook: added (") && out.contains("\nallow rules: added ("));
    assert_eq!(calls().len(), start, "no claude ran");
    assert!(!home.join(".claude.json").exists());

    // An entry that isn't a zerodb mcp command is refused before anything changes, unless
    // --force, which removes it, then adds zerodb's. One with connection flags says how to
    // keep its memory. One from an old binary path is updated without --force.
    let home = dir.0.join("d");
    let state = home.join(".claude.json");
    let rows = [
        (
            "flags",
            json!({"type": "stdio", "command": exe, "args": ["mcp", "--url", "unix:///old"]}),
            "uses --url unix:///old.",
        ),
        (
            "other command",
            json!({"command": "npx", "args": ["-y", "zerodb-mcp"]}),
            "not a zerodb mcp command such as ",
        ),
    ];
    for (case, other, said) in rows {
        std::fs::create_dir_all(&home).unwrap();
        let text = json!({"mcpServers": {"zerodb": other}, "numStartups": 3}).to_string();
        std::fs::write(&state, &text).unwrap();
        let start = calls().len();
        let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
        assert_eq!(code, Some(1), "{case}: {out}");
        assert!(out.contains(said) && out.contains("zerodb connect claude-code --force"), "{out}");
        assert_eq!(std::fs::read_to_string(&state).unwrap(), text, "{case}: entry unchanged");
        assert_eq!(calls().len(), start, "{case}: no claude ran");
        assert!(!home.join(".claude").exists() && !home.join("Library").exists(), "{case}");
        assert!(!home.join(".local").exists(), "{case}: no home made");
    }
    let start = calls().len();
    let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code", "--force"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with(&format!("MCP entry: updated ({})\n", state.display())), "{out}");
    assert_eq!(calls()[start..], [remove.clone(), add(None)]);
    assert_eq!(read(&state)["mcpServers"]["zerodb"], entry(None));
    assert_eq!(read(&state)["numStartups"], 3, "the rest of Claude's state stays");
    let old = json!({"type": "stdio", "command": "/opt/old/zerodb",
        "args": ["mcp", "--agent", "claude-code"], "env": {}});
    std::fs::write(&state, json!({"mcpServers": {"zerodb": old}}).to_string()).unwrap();
    let start = calls().len();
    let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
    assert_eq!(code, Some(0), "old binary: {out}");
    assert!(out.starts_with(&format!("MCP entry: updated ({})\n", state.display())), "{out}");
    assert_eq!(calls()[start..], [remove.clone(), add(None)], "old binary");
    assert_eq!(read(&state)["mcpServers"]["zerodb"], entry(None), "old binary");
    // disconnect leaves an entry zerodb didn't write, and removes the other parts.
    let mine = json!({"command": "npx", "args": ["-y", "zerodb-mcp"]});
    std::fs::write(&state, json!({"mcpServers": {"zerodb": mine}}).to_string()).unwrap();
    let start = calls().len();
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "claude-code"]);
    assert_eq!(code, Some(1), "{out}");
    let said = "MCP entry: skipped, it differs from the entry zerodb writes";
    assert!(out.starts_with(said) && out.contains("claude mcp remove -s user zerodb\n"), "{out}");
    assert!(
        out.contains("\nSessionStart hook: removed (") && out.contains("\nallow rules: removed (")
    );
    assert_eq!((calls().len(), read(&state)["mcpServers"]["zerodb"].clone()), (start, mine));

    // Under ZERODB_HOME, the entry passes it with -e and the hook command sets it, quoted,
    // so the hook reads that home. VS Code on the default home gets a warning, not a refusal.
    let home = dir.0.join("z");
    let zh = home.join("it's home");
    let _holders = support::Holders(vec![zh.join("data")]);
    let zh_str = zh.to_str().unwrap();
    assert_eq!(run(&home, None, &bin, &[], &["connect", "vscode"]).0, Some(0));
    let start = calls().len();
    let (code, out) = run(&home, None, &bin, &[("ZERODB_HOME", &zh)], &["connect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    let said = format!(
        "zerodb: vscode uses the default home, but claude-code now uses ZERODB_HOME {zh_str}. \
         Memory splits"
    );
    assert!(out.contains(&said), "{out}");
    assert_eq!(calls()[start..], [add(Some(zh_str))]);
    assert_eq!(read(&home.join(".claude.json"))["mcpServers"]["zerodb"], entry(Some(zh_str)));
    let settings = read(&home.join(".claude/settings.json"));
    let moved = &settings["hooks"]["SessionStart"][0]["hooks"][0]["command"];
    let prefix = format!("ZERODB_HOME='{}' ", zh_str.replace('\'', r"'\''"));
    assert_eq!(*moved, format!("{prefix}{command}"));
    let seeded = ["fact", "set", "--key", "user/style", "--content", "short sentences"];
    let (code, _, err) = support::zerodb(&home, &home, &seeded, &[("ZERODB_HOME", zh_str)], b"", 0);
    assert_eq!(code, Some(0), "{err}");
    std::fs::write(home.join("stdin"), support::cwd(&home)).unwrap();
    let mut sh = support::command("sh");
    sh.args(["-c", &format!("{} < stdin", moved.as_str().unwrap())]);
    sh.current_dir(&home).env("HOME", &home).stdin(Stdio::null());
    let (status, out) = Process::start(&mut sh).wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let out: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "SessionStart", "{out}");
    let block = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(block.contains("\n- user/style: short sentences"), "{block}");
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.starts_with("MCP entry: removed (") && out.contains("\nSessionStart hook: removed (")
    );

    // --no-hook and --notes are for claude-code only.
    let usage = [
        &["vscode", "--no-hook"][..],
        &["copilot", "--notes", "ask"],
        &["claude-code", "--notes", "maybe"],
    ];
    for args in usage {
        let (code, out) = run(&dir.0, None, &empty, &[], &[&["connect"], args].concat());
        assert_eq!(code, Some(2), "{args:?}: {out}");
    }
    assert!(calls().iter().all(|call| call[..2] != ["mcp", "get"]), "claude mcp get never runs");
}
