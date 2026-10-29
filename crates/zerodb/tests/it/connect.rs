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
/// is connected. A hook file zerodb did not write stays. The quoted path runs in `sh`.
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
    for (row, var) in [None, Some("COPILOT_HOME")].into_iter().enumerate() {
        let home = dir.0.join(row.to_string());
        let hooks = var.map_or(home.join(".copilot"), |_| home.join("c")).join("hooks");
        let file = hooks.join("zerodb.json");
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
        let before = (files(&hooks), std::fs::metadata(&file).unwrap().modified().unwrap());
        let out = run(&["connect", "copilot"]);
        assert!(out.ends_with(&format!("\nthe sessionStart hook is already in place ({shown})\n")));
        let after = (files(&hooks), std::fs::metadata(&file).unwrap().modified().unwrap());
        assert_eq!(after, before, "{var:?}: nothing written");

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
        let config =
            if cfg!(target_os = "macos") { "Library/Application Support" } else { ".config" };
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
}
