use crate::support::{self, chmod, files, mkfifo, Dir, Process};
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Stdio;

/// `connect` adds an owner-only file, a rerun writes nothing, and an older entry is updated.
/// An entry with connection flags is replaced only with `--force`. `disconnect` removes only
/// AtticDB's entry. Rewrites keep a backup and the file mode. A file that is not a JSON
/// object, or a symlink, stays as it was. No token reaches any output.
#[test]
fn connect_edits_only_the_atticdb_entry() {
    let dir = Dir::new();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
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
            let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
            command.args(args).current_dir(&dir.0).env("HOME", &home).env_remove("ATTICDB_TOKEN");
            command.envs(var.map(|var| (var, home.join("c"))));
            let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
            printed.push_str(&out);
            (status.code(), out)
        };
        let read = || serde_json::from_slice::<Value>(&std::fs::read(&file).unwrap()).unwrap();
        let mode = || std::fs::metadata(&file).unwrap().mode() & 0o777;
        let backups =
            || files(&parent).into_keys().filter(|n| n.contains(".atticdb-backup-")).count();
        // Copilot's hook and instructions directories sit next to its config.
        let shared = ["hooks", "instructions"];
        let listed = || files(&parent).into_keys().filter(|n| !shared.contains(&&**n)).count();

        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let first = format!("connected atticdb to {name}: {}", file.display());
        assert_eq!(out.lines().next(), Some(first.as_str()), "{case}");
        assert_eq!(read(), json!({section: {"atticdb": entry}}), "{case}: created");
        let pretty = serde_json::to_string_pretty(&read()).unwrap() + "\n";
        assert_eq!(std::fs::read_to_string(&file).unwrap(), pretty, "{case}: 2-space indent");
        assert_eq!(listed(), 1, "{case}: no backup or temp file");
        assert_eq!(mode(), 0o600, "{case}: a new file is owner-only");
        let parent_mode = std::fs::metadata(&parent).unwrap().mode() & 0o777;
        assert_eq!(parent_mode, 0o700, "{case}: new directories are private");

        let before = (files(&parent), std::fs::metadata(&file).unwrap().modified().unwrap());
        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let first = format!("atticdb is already connected to {name} ({})", file.display());
        assert_eq!(out.lines().next(), Some(first.as_str()), "{case}");
        let after = (files(&parent), std::fs::metadata(&file).unwrap().modified().unwrap());
        assert_eq!(after, before, "{case}: nothing written");

        // An entry with any connection flag keeps its store: refused unless `--force`.
        let force = format!("atticdb connect {name} --force");
        for flag in ["--url", "--data-dir", "--config", "--token-file", "--tenant"] {
            let mut flagged = read();
            flagged[section]["atticdb"]["args"] = json!(["mcp", flag, "/old"]);
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
        assert!(out.starts_with(&format!("updated atticdb in {name}: ")), "{case}: {out}");
        assert_eq!((read()[section]["atticdb"].clone(), backups()), (entry.clone(), 1), "{case}");

        // An older entry, another server and another top-level key, added by hand.
        let mut edited = read();
        edited[section]["atticdb"]["args"] = json!(["mcp"]);
        edited[section]["other"] = json!({"command": "other"});
        edited["editor.fontSize"] = json!(14);
        let edited_bytes = serde_json::to_vec(&edited).unwrap();
        std::fs::write(&file, &edited_bytes).unwrap();
        chmod(&file, 0o600);
        let (code, out) = run(&["connect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let prefix = format!("updated atticdb in {name}: {} (backup: ", file.display());
        let first = out.lines().next().unwrap();
        assert!(first.starts_with(&prefix) && first.ends_with(", keys are now sorted)"), "{out}");
        edited[section]["atticdb"] = entry;
        assert_eq!(read(), edited, "{case}: only the entry changed");
        let backup = first[prefix.len()..].split(", ").next().unwrap().to_owned();
        assert_eq!(std::fs::read(&backup).unwrap(), edited_bytes, "{case}: backup");
        let backup_mode = std::fs::metadata(&backup).unwrap().mode() & 0o777;
        assert_eq!(backup_mode, 0o600, "{case}: the backup keeps the mode");
        assert_eq!((backups(), mode()), (2, 0o600), "{case}: backup, mode kept");

        let (code, out) = run(&["disconnect", name]);
        assert_eq!(code, Some(0), "{case}: {out}");
        assert!(out.starts_with(&format!("disconnected atticdb from {name}: ")), "{case}: {out}");
        edited[section].as_object_mut().unwrap().remove("atticdb");
        assert_eq!(read(), edited, "{case}: the other server and key stay");
        assert_eq!((backups(), mode()), (3, 0o600), "{case}: another backup, mode kept");
        assert_eq!(listed(), 4, "{case}: no temp file left");
        let before = files(&parent);
        let (code, out) = run(&["disconnect", name]);
        assert_eq!((code, out), (Some(0), format!("atticdb is not connected to {name}\n")));
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
        let home_dir = home.join(data).join("atticdb");
        let config: Value =
            serde_json::from_slice(&std::fs::read(home_dir.join("atticdb.json")).unwrap()).unwrap();
        let token = config["tenants"][0]["token"].as_str().unwrap();
        let leaked = files(&parent).into_values().flatten();
        let mut seen = leaked.chain([printed.into_bytes()]);
        let token = token.as_bytes();
        assert!(!seen.any(|bytes| bytes.windows(token.len()).any(|w| w == token)), "{case}: leak");
        assert_eq!(files(&home_dir.join("data")).len(), 0, "{case}: no server started");
    }
    let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
    let (status, out) =
        Process::start(command.args(["connect", "emacs"]).env("HOME", &dir.0).stdin(Stdio::null()))
            .wait();
    assert_eq!(status.code(), Some(2), "{out}");
    assert!(out.contains("atticdb connect <vscode|copilot>"), "{out}");
}

/// `connect` writes 1 hook file and 1 instructions file for both harnesses, and `disconnect`
/// removes them once neither is connected. A hook file AtticDB did not write stays, and so
/// does an instructions file without the marker line, which connect doesn't overwrite, or an
/// edited one. A symlinked instructions file is left alone and doesn't stop the hook. Another instructions file that
/// mentions AtticDB stops ours. The quoted path runs in `sh`. Under `ATTICDB_HOME`, both
/// entries and the hook pass it on, and a path with a `$` is refused. Both entries must pass
/// the same home, as the hook reads 1. VS Code reads Copilot CLI's entry too, so while that
/// exists, VS Code's `mcp.json` holds none, whichever is connected first.
#[test]
fn connect_writes_the_session_start_hook() {
    let dir = Dir::new();
    // A binary path with a quote, which the hook command must escape.
    let bin = dir.0.join("it's");
    std::fs::create_dir(&bin).unwrap();
    let exe = bin.join("atticdb");
    std::fs::copy(env!("CARGO_BIN_EXE_atticdb"), &exe).unwrap();
    let exe = std::fs::canonicalize(exe).unwrap();
    let bash = format!("'{}' hook session-start", exe.to_str().unwrap().replace('\'', r"'\''"));
    let wanted = json!({"version": 1, "hooks": {"sessionStart": [
        {"type": "command", "bash": bash, "timeoutSec": 5}]}});
    let config = if cfg!(target_os = "macos") { "Library/Application Support" } else { ".config" };
    let env_of = |path: &std::path::Path, section: &str| {
        let config: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        config[section]["atticdb"]["env"].clone()
    };
    let rows = [None, Some("COPILOT_HOME"), Some("ATTICDB_HOME")];
    for (row, var) in rows.into_iter().enumerate() {
        let home = dir.0.join(row.to_string());
        let copilot =
            if var == Some("COPILOT_HOME") { home.join("c") } else { home.join(".copilot") };
        let hooks = copilot.join("hooks");
        let file = hooks.join("atticdb.json");
        let instructions = copilot.join("instructions");
        let guide = instructions.join("atticdb.instructions.md");
        let said_guide =
            |said: &str| format!("\n{said} the atticdb instructions: {}\n", guide.display());
        let moved = var == Some("ATTICDB_HOME");
        let env = if moved { json!({"ATTICDB_HOME": home.join("c")}) } else { Value::Null };
        let mut wanted = wanted.clone();
        if moved {
            wanted["hooks"]["sessionStart"][0]["env"] = env.clone();
        }
        let run = |args: &[&str]| {
            let mut command = support::command(&exe);
            command.args(args).current_dir(&dir.0).env("HOME", &home).env_remove("ATTICDB_TOKEN");
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
            "---\napplyTo: \"**\"\ndescription: \"How to use AtticDB, this user's long-term \
             memory\"\n---\n<!-- written by atticdb connect -->\n\n{}\n",
            atticdb::mcp::INSTRUCTIONS
        );
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), text, "{var:?}");
        let session = "Pass session=<repo> on memory_append and memory_recent.";
        assert!(text.contains(session), "{var:?}: the guide names the session");
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
            format!("\nthe atticdb instructions are already in place ({})\n", guide.display());
        assert!(out.contains(&in_place), "{var:?}: {out}");
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), text, "{var:?}: kept");
        let entry = env_of(&copilot.join("mcp-config.json"), "mcpServers");
        assert_eq!(entry, env, "{var:?}: Copilot's entry");
        // VS Code reads Copilot CLI's entry too, so connecting Copilot CLI removes VS Code's.
        // Only the entry's bytes go, with a backup.
        let (copilot_file, vscode_dir) =
            (copilot.join("mcp-config.json"), vscode.parent().unwrap());
        let removed =
            format!("\nremoved the duplicate atticdb entry from {} (backup: ", vscode.display());
        assert!(out.contains(&removed), "{var:?}: {out}");
        let now = || std::fs::read_to_string(&vscode).ok();
        assert_eq!(now().as_deref(), Some("{\n  \"servers\": {}\n}\n"), "{var:?}");
        // While Copilot CLI has its entry, connecting VS Code adds none to mcp.json, and
        // removes an old one, leaving the other bytes as they were. --force writes it anyway,
        // for VS Code builds older than 1.144. The hook and instructions stay.
        // (case, args, mcp.json before, after, what connect says first)
        let reads = format!(
            ". VS Code 1.144 and later read it from {}. Older VS Code: update it, or run atticdb \
             connect vscode --force\n",
            copilot_file.display()
        );
        let old = "{\n    \"inputs\": [],\n    \"servers\": {\n        \"other\": {\"command\": \"x\"},\n        \
                   \"atticdb\": {\"command\": \"/old/atticdb\", \"args\": [\"mcp\"]}\n    }\n}\n";
        let others =
            "{\n    \"inputs\": [],\n    \"servers\": {\n        \"other\": {\"command\": \"x\"}\n    }\n}\n";
        let mut forced: Value = serde_json::from_str(others).unwrap();
        forced["servers"]["atticdb"] = json!({"type": "stdio", "command": exe,
            "args": ["mcp", "--agent", "vscode-copilot"]});
        if moved {
            forced["servers"]["atticdb"]["env"] = env.clone();
        }
        let forced = serde_json::to_string_pretty(&forced).unwrap() + "\n";
        let (plain, force) = (&["connect", "vscode"][..], &["connect", "vscode", "--force"][..]);
        let rows = [
            ("old entry", plain, Some(old), Some(others), "removed the duplicate atticdb entry"),
            ("no entry", plain, Some(others), Some(others), ": no atticdb entry needed"),
            ("no file", plain, None, None, ": no atticdb entry needed"),
            ("--force", force, Some(others), Some(forced.as_str()), "connected atticdb to vscode"),
        ];
        for (case, args, before, after, said) in rows {
            match before {
                Some(before) => std::fs::write(&vscode, before).unwrap(),
                None => std::fs::remove_file(&vscode).unwrap(),
            }
            let shared = (files(&hooks), files(&instructions));
            let out = run(args);
            let first = out.lines().next().unwrap_or_default();
            assert!(first.contains(said), "{var:?} {case}: {out}");
            assert_eq!(out.contains(&reads), args == plain, "{var:?} {case}: {out}");
            assert_eq!(now().as_deref(), after, "{var:?} {case}: mcp.json");
            assert_eq!((files(&hooks), files(&instructions)), shared, "{var:?} {case}: kept");
        }
        let names = files(vscode_dir).into_keys();
        let backups = names.filter(|name| name.contains(".atticdb-backup-")).count();
        assert_eq!((backups, files(vscode_dir).len()), (3, 4), "{var:?}: 3 backups, no temp file");

        // VS Code still reads Copilot CLI's entry, so disconnecting VS Code says so. The hook
        // and instructions stay.
        std::fs::write(&vscode, old).unwrap();
        let out = run(&["disconnect", "vscode"]);
        assert!(out.starts_with("disconnected atticdb from vscode: "), "{var:?}: {out}");
        let still = format!(
            "\nVS Code 1.144 and later still read atticdb from {}. To remove",
            copilot_file.display()
        );
        assert!(out.contains(&still), "{var:?}: {out}");
        let servers: Value = serde_json::from_slice(&std::fs::read(&vscode).unwrap()).unwrap();
        assert_eq!(servers["servers"], json!({"other": {"command": "x"}}), "{var:?}");
        let kept = format!("\nkept the sessionStart hook ({shown}): atticdb is still connected");
        assert!(out.contains(&kept), "{var:?}: {out}");
        assert_eq!(files(&hooks), before.0, "{var:?}: kept");
        let kept =
            format!("\nkept the atticdb instructions ({}): atticdb is still", guide.display());
        assert!(out.contains(&kept) && guide.exists(), "{var:?}: {out}");
        let out = run(&["disconnect", "copilot"]);
        assert!(out.ends_with(&format!("\nremoved the sessionStart hook: {shown}\n")), "{out}");
        assert_eq!(files(&hooks).len(), 0, "{var:?}: removed, no backup");
        assert!(out.contains(&said_guide("removed")), "{var:?}: {out}");
        assert_eq!(files(&instructions).len(), 0, "{var:?}: instructions removed, no backup");

        // Without the marker line, the instructions file stays, and connect doesn't overwrite it.
        // Another instructions file that mentions AtticDB, in any case, stops connect from
        // writing ours. Its backup doesn't.
        std::fs::write(&guide, "mine\n").unwrap();
        let out = run(&["disconnect", "copilot"]);
        let alone = format!("left {} alone: it has no line <!-- written by", guide.display());
        assert!(out.contains(&alone), "{var:?}: {out}");
        assert_eq!(std::fs::read_to_string(&guide).unwrap(), "mine\n", "{var:?}: no marker");
        let out = run(&["connect", "copilot"]);
        let skipped = format!("skipped the atticdb instructions: {} has no line", guide.display());
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
        let other = instructions.join("atticdb-memory.instructions.md");
        let backup = instructions.join("atticdb-memory.instructions.md.bak-1");
        std::fs::write(&other, "Save notes in AtticDB.\n").unwrap();
        std::fs::write(&backup, "atticdb").unwrap();
        // A FIFO is not read, so it can't block connect.
        let fifo = instructions.join("x.instructions.md");
        mkfifo(&fifo);
        let out = run(&["connect", "copilot"]);
        let skipped = format!("skipped the atticdb instructions: {} already", other.display());
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
            json!(r"'/opt/it'\''s/atticdb' hook session-start");
        let other = serde_json::to_string_pretty(&other).unwrap() + "\n";
        // A config with comments and no AtticDB entry does not keep the hook.
        std::fs::write(home.join(config).join("Code/User/mcp.json"), "// mine\n{}\n").unwrap();
        let rows = [
            (other.as_str(), "removed the sessionStart hook", None),
            ("{}", "alone: it differs from the hook atticdb writes", Some("{}")),
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
    command.args(["connect", "copilot"]).env("HOME", &home).env("ATTICDB_HOME", &moved);
    let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
    assert_eq!(status.code(), Some(1), "{out}");
    assert!(out.contains("holds a $") && out.contains("Nothing changed"), "{out}");
    assert!(!home.exists() && !moved.exists(), "nothing written: {out}");

    // The hook reads 1 home, so the 2 entries must pass the same one. A mismatch either way is
    // refused and changes nothing. --force goes ahead and names the harness on the other home.
    let (home, zh) = (dir.0.join("4"), dir.0.join("4/zh"));
    let (vscode, copilot) = (home.join(config).join("Code/User/mcp.json"), home.join(".copilot"));
    let (entry, hook) = (copilot.join("mcp-config.json"), copilot.join("hooks/atticdb.json"));
    let connect = |args: &[&str], moved: bool| {
        let mut command = support::command(&exe);
        command.args(args).current_dir(&dir.0).env("HOME", &home);
        command.envs(moved.then_some(("ATTICDB_HOME", &zh)));
        let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
        (status.code(), out)
    };
    let now = || [&vscode, &entry, &hook].map(|path| std::fs::read(path).ok());
    let hook_env = || {
        let hook: Value = serde_json::from_slice(&std::fs::read(&hook).unwrap()).unwrap();
        hook["hooks"]["sessionStart"][0]["env"].clone()
    };
    let (moved, fix) = (format!("ATTICDB_HOME {}", zh.display()), "Nothing changed. Connect");
    assert_eq!(connect(&["connect", "vscode"], false).0, Some(0), "VS Code on the default home");
    let before = now();
    let (code, out) = connect(&["connect", "copilot"], true);
    let said = format!("vscode's atticdb entry uses the default home, but this would use {moved}.");
    assert_eq!(code, Some(1), "mismatch: {out}");
    assert!(out.contains(&said) && out.contains(fix), "mismatch: {out}");
    assert!(out.contains("vscode again under the same ATTICDB_HOME, or disconnect"), "{out}");
    assert_eq!(now(), before, "mismatch: no file changed");
    assert!(!zh.exists(), "mismatch: no home made");
    let (code, out) = connect(&["connect", "copilot", "--force"], true);
    assert_eq!(code, Some(0), "--force: {out}");
    // VS Code's entry on the other home goes, as VS Code reads Copilot CLI's. So no home splits.
    assert!(out.contains("\nremoved the duplicate atticdb entry from "), "--force: {out}");
    assert!(!out.contains("still uses"), "--force: {out}");
    let env = json!({"ATTICDB_HOME": zh});
    assert_eq!((env_of(&entry, "mcpServers"), hook_env()), (env.clone(), env.clone()), "--force");
    assert_eq!(env_of(&vscode, "servers"), Value::Null, "--force: VS Code's entry removed");
    // The other way round: VS Code again with ATTICDB_HOME unset. --force names Copilot CLI's
    // home.
    let before = now();
    let (code, out) = connect(&["connect", "vscode"], false);
    let said =
        format!("copilot's atticdb entry uses {moved}, but this would use the default home.");
    assert_eq!(code, Some(1), "unset: {out}");
    assert!(out.contains(&said) && out.contains(fix), "unset: {out}");
    assert_eq!(now(), before, "unset: no file changed");
    let (code, out) = connect(&["connect", "vscode", "--force"], false);
    let said = format!(
        "copilot still uses {moved}, but the shared sessionStart hook now reads the default home."
    );
    assert_eq!(code, Some(0), "unset --force: {out}");
    assert!(out.contains(&said), "unset --force: {out}");
    assert_eq!(hook_env(), Value::Null, "unset --force");
    // --force writes VS Code's own entry, for builds older than 1.144.
    let config: Value = serde_json::from_slice(&std::fs::read(&vscode).unwrap()).unwrap();
    assert!(config["servers"]["atticdb"].is_object(), "unset --force: {config}");
    // Matching homes connect cleanly. Without --force, VS Code's own entry goes again.
    let (code, out) = connect(&["connect", "vscode"], true);
    assert_eq!(code, Some(0), "matching: {out}");
    assert!(out.starts_with("removed the duplicate atticdb entry from "), "matching: {out}");
    assert!(!out.contains("still uses"), "matching: {out}");
    assert_eq!((env_of(&vscode, "servers"), hook_env()), (Value::Null, env), "matching");
    // A hand-made VS Code entry in a file with comments passes no ATTICDB_HOME, and an empty
    // one counts as unset. Both are on the default home, as Copilot is about to be. The empty
    // one is removed. A file with comments isn't strict JSON, so only its text was searched,
    // and an entry there may be commented out. It stays, connect says it may hold an entry to
    // remove by hand, and exits 1.
    let by_hand = "may hold an atticdb entry, but it isn't strict JSON: invalid JSON at byte ";
    let rows = [
        ("empty", "{\"servers\": {\"atticdb\": {\"command\": \"atticdb\", \"env\": {\"ATTICDB_HOME\": \"\"}}}}", 0, "{\"servers\": {}}"),
        ("commented out", "{\n  // \"atticdb\": {\"command\": \"atticdb\"},\n  \"servers\": {}\n}\n", 1, ""),
        ("comments", "// mine\n{\"servers\": {\"atticdb\": {\"command\": \"atticdb\"}}}\n", 1, ""),
    ];
    for (case, text, wanted, left) in rows {
        std::fs::write(&vscode, text).unwrap();
        let (code, out) = connect(&["connect", "copilot"], false);
        assert_eq!(code, Some(wanted), "{case}: {out}");
        assert!(!out.contains("still uses"), "{case}: {out}");
        assert_eq!(out.contains(by_hand), wanted == 1, "{case}: {out}");
        let left = if wanted == 1 { text } else { left };
        assert_eq!(std::fs::read_to_string(&vscode).unwrap(), left, "{case}");
        assert_eq!(
            (env_of(&entry, "mcpServers"), hook_env()),
            (Value::Null, Value::Null),
            "{case}"
        );
    }
    // Copilot's own entry with --data-dir, under a home that also splits from VS Code's. The
    // flags refusal shows first, so --force is never advised past a warning not yet shown.
    let mut flagged: Value = serde_json::from_slice(&std::fs::read(&entry).unwrap()).unwrap();
    flagged["mcpServers"]["atticdb"]["args"] = json!(["mcp", "--data-dir", "/old"]);
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
        command.env("COPILOT_HOME", &copilot).env("ATTICDB_HOME", &zh);
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
    assert!(out.contains(&format!("atticdb: {said}")), "HOME unset --force: {out}");
    let env = json!({"ATTICDB_HOME": zh});
    let hook: Value =
        serde_json::from_slice(&std::fs::read(copilot.join("hooks/atticdb.json")).unwrap())
            .unwrap();
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
/// SessionStart and PostCompact hooks and the allow rules to `settings.json`, in its config
/// dir. A rerun writes nothing. `disconnect` removes them, leaving a hand-kept file byte for
/// byte as it was. `--notes ask` leaves out `memory_append`, and `--no-hook` the hooks. A part
/// that can't be applied is skipped with what to add by hand, the others still apply, and the
/// exit code is 1. A different entry is refused unless `--force`. `claude mcp get` never runs.
#[test]
fn connect_claude_code_adds_its_parts() {
    let dir = Dir::new();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
    let exe = exe.to_str().unwrap().to_owned();
    let (bin, empty) = (dir.0.join("bin"), dir.0.join("empty"));
    let log = support::fake_claude(&bin);
    std::fs::create_dir(&empty).unwrap();
    let calls = || -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    };
    let add = |home: Option<&str>| {
        // -e takes a list, so it goes after the name. The fake fails it before the name.
        let mut args = vec!["mcp", "add", "-s", "user", "atticdb"];
        let env = home.map(|home| format!("ATTICDB_HOME={home}"));
        args.extend(env.iter().flat_map(|env| ["-e", env.as_str()]));
        args.extend(["--", &exe, "mcp", "--agent", "claude-code"]);
        args.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let remove = ["mcp", "remove", "-s", "user", "atticdb"].map(String::from).to_vec();
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    let command = format!("{quoted} hook session-start --harness claude-code");
    let group = json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]});
    let compact = format!("{quoted} hook post-compact --harness claude-code");
    let compact = json!({"hooks": [{"type": "command", "command": compact, "timeout": 5}]});
    let reads =
        ["mcp__atticdb__memory_search", "mcp__atticdb__memory_recent", "mcp__atticdb__fact_get"];
    let reads = [&reads[..], &["mcp__atticdb__fact_list", "mcp__atticdb__fact_history"]].concat();
    let append = "mcp__atticdb__memory_append";
    let run =
        |home: &Path, config: Option<&Path>, path: &Path, env: &[(&str, &Path)], args: &[&str]| {
            let mut command = support::command(&exe);
            command.args(args).current_dir(&dir.0).env("HOME", home).env("PATH", path);
            command
                .env_remove("ATTICDB_TOKEN")
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
        let env = home.map_or(json!({}), |home| json!({"ATTICDB_HOME": home}));
        let args = ["mcp", "--agent", "claude-code"];
        json!({"type": "stdio", "command": exe, "args": args, "env": env})
    };
    let next = "Start a new Claude Code session to load atticdb.";

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
        assert_eq!(lines[2], format!("PostCompact hook: added ({shown}{backup})"), "{case}");
        assert_eq!(lines[3], format!("allow rules: added ({shown}{backup})"), "{case}");
        assert_eq!(lines[4..], [next], "{case}");
        assert_eq!(calls()[start..], [add(None)], "{case}");
        assert_eq!(read(&state)["mcpServers"]["atticdb"], entry(None), "{case}");
        let mut wanted = original.clone();
        push(&mut wanted, ["hooks", "SessionStart"], std::slice::from_ref(&group));
        push(&mut wanted, ["hooks", "PostCompact"], std::slice::from_ref(&compact));
        push(&mut wanted, ["permissions", "allow"], &strings(&[&reads[..], &[append]].concat()));
        assert_eq!(read(&settings), wanted, "{case}");
        let mode = std::fs::metadata(&settings).unwrap().mode() & 0o777;
        assert_eq!(mode, if kept.is_some() { 0o644 } else { 0o600 }, "{case}: mode");
        if kept.is_none() {
            // serde_json sorts keys, but the file keeps them in the order they went in.
            let sorted = serde_json::to_string_pretty(&wanted).unwrap() + "\n";
            let (head, rest) = sorted.split_once("\n    \"PostCompact\": ").unwrap();
            let (compact, rest) = rest.split_once(",\n    \"SessionStart\": ").unwrap();
            let (start, tail) = rest.split_once("\n  },\n  \"permissions\"").unwrap();
            let pretty = format!(
                "{head}\n    \"SessionStart\": {start},\n    \"PostCompact\": {compact}\n  }},\n  \
                 \"permissions\"{tail}"
            );
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
             PostCompact hook: unchanged ({shown})\nallow rules: unchanged ({shown})\n{next}\n"
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
        assert!(lines[2].starts_with(&format!("PostCompact hook: removed ({shown}, backup: ")));
        assert!(lines[3].starts_with(&format!("allow rules: removed ({shown}, backup: ")));
        assert_eq!(lines.len(), 4, "{case}: {out}");
        assert_eq!(calls().last(), Some(&remove), "{case}");
        assert_eq!(read(&state)["mcpServers"], json!({}), "{case}");
        // An emptied SessionStart array stays, but disconnect drops the PostCompact key it emptied.
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
        assert!(!out.contains("PostCompact hook"), "{case}: {out}");
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
    assert_eq!(read(&home.join(".claude.json"))["mcpServers"]["atticdb"], entry(None));

    // Without claude on PATH, the MCP part is skipped with the command to run, and the
    // settings parts still go in.
    let home = dir.0.join("m");
    let start = calls().len();
    let (code, out) = run(&home, None, &empty, &[], &["connect", "claude-code"]);
    assert_eq!(code, Some(1), "{out}");
    let skipped =
        "MCP entry: skipped, claude is not on PATH. Run: claude mcp add -s user atticdb -- ";
    assert!(out.starts_with(skipped), "{out}");
    assert!(out.lines().next().unwrap().ends_with(" mcp --agent claude-code"), "{out}");
    assert!(out.contains("\nSessionStart hook: added (") && out.contains("\nallow rules: added ("));
    assert_eq!(calls().len(), start, "no claude ran");
    assert!(!home.join(".claude.json").exists());

    // An entry that isn't an `atticdb mcp` command is refused before anything changes, unless
    // --force, which removes it, then adds AtticDB's. One with connection flags says how to
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
            json!({"command": "npx", "args": ["-y", "atticdb-mcp"]}),
            "not an atticdb mcp command such as ",
        ),
    ];
    for (case, other, said) in rows {
        std::fs::create_dir_all(&home).unwrap();
        let text = json!({"mcpServers": {"atticdb": other}, "numStartups": 3}).to_string();
        std::fs::write(&state, &text).unwrap();
        let start = calls().len();
        let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
        assert_eq!(code, Some(1), "{case}: {out}");
        assert!(out.contains(said) && out.contains("atticdb connect claude-code --force"), "{out}");
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
    assert_eq!(read(&state)["mcpServers"]["atticdb"], entry(None));
    assert_eq!(read(&state)["numStartups"], 3, "the rest of Claude's state stays");
    // An entry from an old binary path, or one whose env could pick another home than the
    // hook's, is AtticDB's by its shape, so it is updated without --force.
    let old = json!({"type": "stdio", "command": "/opt/old/atticdb",
        "args": ["mcp", "--agent", "claude-code"], "env": {}});
    let mut other_env = entry(None);
    other_env["env"] = json!({"XDG_DATA_HOME": "/x"});
    for (case, other) in [("old binary", &old), ("XDG_DATA_HOME", &other_env)] {
        std::fs::write(&state, json!({"mcpServers": {"atticdb": other}}).to_string()).unwrap();
        let start = calls().len();
        let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let said = format!("MCP entry: updated ({})\n", state.display());
        assert!(out.starts_with(&said), "{case}: {out}");
        assert_eq!(calls()[start..], [remove.clone(), add(None)], "{case}");
        assert_eq!(read(&state)["mcpServers"]["atticdb"], entry(None), "{case}");
    }
    // claude removed the old entry, but its add failed: the line names the file that keeps
    // the old entry, shows its env names but no values, and says to re-run connect, which
    // then adds AtticDB's.
    let mut secret = old.clone();
    secret["env"] = json!({"API_KEY": "sk-secret-0123456789"});
    std::fs::write(&state, json!({"mcpServers": {"atticdb": secret}}).to_string()).unwrap();
    let start = calls().len();
    let fails = [("FAKE_CLAUDE", Path::new("add-fails"))];
    let (code, out) = run(&home, None, &bin, &fails, &["connect", "claude-code"]);
    assert_eq!(code, Some(1), "{out}");
    let said = "The old entry was removed. It ran \"/opt/old/atticdb\" \
                [\"mcp\",\"--agent\",\"claude-code\"] with env names [\"API_KEY\"]. The whole \
                entry is saved in ";
    assert!(out.starts_with("MCP entry: failed, claude mcp add ") && out.contains(said), "{out}");
    assert!(out.contains("Re-run atticdb connect claude-code"), "{out}");
    assert!(!out.contains("sk-secret") && !out.contains("claude mcp remove"), "{out}");
    let kept = out.split(said).nth(1).unwrap().split(". Re-run").next().unwrap();
    assert!(
        kept.starts_with(&format!("{}/atticdb-mcp-entry.json.", home.join(".claude").display()))
    );
    assert_eq!(read(Path::new(kept)), secret, "the whole entry is kept");
    assert_eq!(std::fs::metadata(kept).unwrap().mode() & 0o777, 0o600);
    assert_eq!(calls()[start..], [remove.clone(), add(None)]);
    assert_eq!(read(&state)["mcpServers"], json!({}));
    let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("MCP entry: added ("), "{out}");
    // disconnect leaves an entry AtticDB didn't write, and removes the other parts.
    let mine = json!({"command": "npx", "args": ["-y", "atticdb-mcp"]});
    std::fs::write(&state, json!({"mcpServers": {"atticdb": mine}}).to_string()).unwrap();
    let start = calls().len();
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "claude-code"]);
    assert_eq!(code, Some(1), "{out}");
    let said = "MCP entry: skipped, it differs from the entry atticdb writes";
    assert!(out.starts_with(said) && out.contains("claude mcp remove -s user atticdb\n"), "{out}");
    assert!(
        out.contains("\nSessionStart hook: removed (") && out.contains("\nallow rules: removed (")
    );
    assert_eq!((calls().len(), read(&state)["mcpServers"]["atticdb"].clone()), (start, mine));

    // Under ATTICDB_HOME, the entry passes it with -e and the hook command sets it, quoted,
    // so the hook reads that home. VS Code on the default home gets a warning, not a refusal.
    let home = dir.0.join("z");
    let zh = home.join("it's home");
    let _holders = support::Holders(vec![zh.join("data")]);
    let zh_str = zh.to_str().unwrap();
    assert_eq!(run(&home, None, &bin, &[], &["connect", "vscode"]).0, Some(0));
    let start = calls().len();
    let (code, out) = run(&home, None, &bin, &[("ATTICDB_HOME", &zh)], &["connect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    let said = format!(
        "atticdb: vscode uses the default home, but claude-code now uses ATTICDB_HOME {zh_str}. \
         Memory splits"
    );
    assert!(out.contains(&said), "{out}");
    assert_eq!(calls()[start..], [add(Some(zh_str))]);
    assert_eq!(read(&home.join(".claude.json"))["mcpServers"]["atticdb"], entry(Some(zh_str)));
    let settings = read(&home.join(".claude/settings.json"));
    let moved = &settings["hooks"]["SessionStart"][0]["hooks"][0]["command"];
    let prefix = format!("ATTICDB_HOME='{}' ", zh_str.replace('\'', r"'\''"));
    assert_eq!(*moved, format!("{prefix}{command}"));
    let seeded = ["fact", "set", "--key", "user/style", "--content", "short sentences"];
    let (code, _, err) =
        support::atticdb(&home, &home, &seeded, &[("ATTICDB_HOME", zh_str)], b"", 0);
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

    // AtticDB's handler in a group with other hooks: connect adds no second group, and
    // disconnect leaves it, says to remove it by hand there, and exits 1.
    let home = dir.0.join("x");
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let shared = json!({"hooks": {"SessionStart": [{"hooks": [
        {"type": "command", "command": "echo hi"},
        {"type": "command", "command": command, "timeout": 5}]}]}});
    std::fs::write(&settings, serde_json::to_string_pretty(&shared).unwrap()).unwrap();
    let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    let said = "\nSessionStart hook: found inside a group with other hooks, so not added again (";
    assert!(out.contains(said), "{out}");
    let start = |path: &Path| read(path)["hooks"]["SessionStart"].clone();
    assert_eq!(start(&settings), shared["hooks"]["SessionStart"], "no second group");
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "claude-code"]);
    assert_eq!(code, Some(1), "{out}");
    let said =
        "\nSessionStart hook: found inside a group with other hooks; remove it by hand there (";
    assert!(out.starts_with("MCP entry: removed (") && out.contains(said), "{out}");
    assert!(out.contains("\nallow rules: removed ("), "{out}");
    assert_eq!(start(&settings), shared["hooks"]["SessionStart"], "the group stays");
    // An older AtticDB handler there is skipped, with advice for that group.
    let mut older = shared.clone();
    older["hooks"]["SessionStart"][0]["hooks"][1]["command"] =
        json!("'/opt/old/atticdb' hook session-start --harness claude-code");
    std::fs::write(&settings, serde_json::to_string_pretty(&older).unwrap()).unwrap();
    let (code, out) = run(&home, None, &bin, &[], &["connect", "claude-code"]);
    assert_eq!(code, Some(1), "{out}");
    let said = format!(
        "an older atticdb hook is inside a group with other hooks. Update the atticdb handler \
         inside that group by hand, to run: {command}\n"
    );
    assert!(out.contains(&said) && !out.contains("Add this group"), "{out}");
    assert_eq!(start(&settings), older["hooks"]["SessionStart"], "nothing added");

    // A binary path with a $, a backtick and a space. A symlink would not do: connect
    // writes the binary's real path. Connect then disconnect round-trip, and the hook command
    // runs in sh.
    let home = dir.0.join("q");
    let _holders = support::Holders(vec![support::data_dir(&home)]);
    let hostile = home.join("a $b`c d");
    std::fs::create_dir_all(&hostile).unwrap();
    let hostile = hostile.join("atticdb");
    std::fs::copy(&exe, &hostile).unwrap();
    let mut command = support::command(&hostile);
    command.args(["connect", "claude-code"]).current_dir(&dir.0).env("HOME", &home);
    command.env("PATH", &bin).env_remove("ATTICDB_TOKEN").stdin(Stdio::null());
    let (status, out) = Process::start(&mut command).wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let settings = home.join(".claude/settings.json");
    let hook = &read(&settings)["hooks"]["SessionStart"][0]["hooks"][0]["command"];
    let path = hostile.to_str().unwrap();
    assert_eq!(*hook, format!("'{path}' hook session-start --harness claude-code"));
    let compact = &read(&settings)["hooks"]["PostCompact"][0]["hooks"][0]["command"];
    assert_eq!(*compact, format!("'{path}' hook post-compact --harness claude-code"));
    assert_eq!(read(&home.join(".claude.json"))["mcpServers"]["atticdb"]["command"], path);
    std::fs::write(home.join("stdin"), support::cwd(&home)).unwrap();
    let mut sh = support::command("sh");
    sh.args(["-c", &format!("{} < stdin", hook.as_str().unwrap())]);
    sh.current_dir(&home).env("HOME", &home).stdin(Stdio::null());
    let (status, out) = Process::start(&mut sh).wait();
    assert_eq!((status.code(), out.lines().next()), (Some(0), Some("{}")), "{out}");
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    let emptied = json!({"hooks": {"SessionStart": []}, "permissions": {"allow": []}});
    assert_eq!(read(&settings), emptied, "{out}");
    assert_eq!(read(&home.join(".claude.json"))["mcpServers"], json!({}));

    // A binary with another file name, as an install may give it, still writes parts that
    // disconnect finds, run from any binary.
    let home = dir.0.join("v");
    std::fs::create_dir(&home).unwrap();
    let versioned = home.join("atticdb-0.1.0");
    std::fs::copy(&exe, &versioned).unwrap();
    let mut command = support::command(&versioned);
    command.args(["connect", "claude-code"]).current_dir(&dir.0).env("HOME", &home);
    command.env("PATH", &bin).env_remove("ATTICDB_TOKEN").stdin(Stdio::null());
    let (status, out) = Process::start(&mut command).wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let state = read(&home.join(".claude.json"));
    assert_eq!(state["mcpServers"]["atticdb"]["command"], versioned.to_str().unwrap());
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    let removed = [
        "MCP entry: removed (",
        "\nSessionStart hook: removed (",
        "\nPostCompact hook: removed (",
        "\nallow rules: removed (",
    ];
    assert!(removed.iter().all(|said| out.contains(said)), "{out}");
    assert_eq!(read(&home.join(".claude.json"))["mcpServers"], json!({}));

    // A relative CLAUDE_CONFIG_DIR would send claude's writes under the current directory.
    // Both verbs refuse it before any change, with exit 2.
    let home = dir.0.join("r");
    let start = calls().len();
    for verb in ["connect", "disconnect"] {
        let (code, out) = run(&home, Some(Path::new("rel")), &bin, &[], &[verb, "claude-code"]);
        assert_eq!(code, Some(2), "{verb}: {out}");
        assert!(out.contains("CLAUDE_CONFIG_DIR rel must be an absolute path"), "{verb}: {out}");
    }
    assert!(!home.exists() && !dir.0.join("rel").exists(), "nothing written");
    assert_eq!(calls().len(), start, "no claude ran");

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

/// A `claude` that runs past the 10 s timeout is killed with everything it started, and the
/// MCP part fails while the others still apply.
#[test]
#[ignore = "waits out the 10 s claude timeout"]
fn connect_claude_code_kills_a_slow_claude() {
    let dir = Dir::new();
    let (bin, home) = (dir.0.join("bin"), dir.0.join("h"));
    support::fake_claude(&bin);
    let marker = dir.0.join("marker");
    let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
    command.args(["connect", "claude-code"]).current_dir(&dir.0).env("HOME", &home);
    command.env("PATH", &bin).env("FAKE_CLAUDE", "slow").env("FAKE_CLAUDE_MARKER", &marker);
    let started = std::time::Instant::now();
    let (status, out) =
        Process::start(command.env_remove("ATTICDB_TOKEN").stdin(Stdio::null())).wait();
    let took = started.elapsed();
    assert_eq!(status.code(), Some(1), "{out}");
    assert!(took >= std::time::Duration::from_secs(10), "{took:?}");
    assert!(
        out.starts_with("MCP entry: failed, claude mcp add ")
            && out.contains("did not finish in 10 s"),
        "{out}"
    );
    assert!(
        out.contains("\nSessionStart hook: added (") && out.contains("\nallow rules: added ("),
        "{out}"
    );
    // The fake's own child would touch the marker at 12 s, unless its group was killed.
    std::thread::sleep(std::time::Duration::from_secs(14).saturating_sub(started.elapsed()));
    assert!(!marker.exists(), "a child of claude outlived the timeout");
}

/// A home for the PostCompact tests: a fake `claude` on `PATH` and an empty `HOME`.
struct Compact {
    dir: Dir,
    exe: String,
}

impl Compact {
    fn new() -> Self {
        let dir = Dir::new();
        support::fake_claude(&dir.0.join("bin"));
        let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
        Self { dir, exe: exe.to_str().unwrap().to_owned() }
    }

    /// Runs `atticdb` with `args`, and returns its exit code and stdout.
    fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        let mut command = support::command(&self.exe);
        command.args(args).current_dir(&self.dir.0).env("HOME", self.dir.0.join("h"));
        command.env("PATH", self.dir.0.join("bin")).env_remove("ATTICDB_TOKEN");
        let (status, out) =
            Process::start(command.env_remove("CLAUDE_CONFIG_DIR").stdin(Stdio::null())).wait();
        (status.code(), out)
    }

    fn settings(&self) -> std::path::PathBuf {
        self.dir.0.join("h/.claude/settings.json")
    }

    /// The groups in `hooks.<event>` of `settings.json`.
    fn groups(&self, event: &str) -> Vec<Value> {
        let root: Value = serde_json::from_slice(&std::fs::read(self.settings()).unwrap()).unwrap();
        root["hooks"][event].as_array().cloned().unwrap_or_default()
    }

    /// The PostCompact group AtticDB adds, with no matcher.
    fn group(&self) -> Value {
        let quoted = format!("'{}'", self.exe.replace('\'', r"'\''"));
        let command = format!("{quoted} hook post-compact --harness claude-code");
        json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]})
    }
}

/// `connect claude-code` adds 1 PostCompact group that runs `hook post-compact`.
#[test]
fn connect_adds_post_compact_hook() {
    let it = Compact::new();
    let (code, out) = it.run(&["connect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    let shown = it.settings().display().to_string();
    assert!(out.contains(&format!("\nPostCompact hook: added ({shown})\n")), "{out}");
    assert_eq!(it.groups("PostCompact"), [it.group()]);
    assert_eq!(it.groups("SessionStart").len(), 1, "the SessionStart group too");
}

/// A rerun of connect adds no second PostCompact group, and says it is unchanged.
#[test]
fn connect_post_compact_is_idempotent() {
    let it = Compact::new();
    assert_eq!(it.run(&["connect", "claude-code"]).0, Some(0));
    let before = std::fs::read(it.settings()).unwrap();
    let (code, out) = it.run(&["connect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    let shown = it.settings().display().to_string();
    assert!(out.contains(&format!("\nPostCompact hook: unchanged ({shown})\n")), "{out}");
    assert_eq!(std::fs::read(it.settings()).unwrap(), before, "nothing written");
    assert_eq!(it.groups("PostCompact"), [it.group()]);
}

/// A user's own PostCompact group survives connect and disconnect, and keeps its place.
#[test]
fn connect_keeps_user_post_compact_groups() {
    let it = Compact::new();
    let theirs = json!({"matcher": "manual", "hooks": [{"type": "command", "command": "say hi"}]});
    let kept = json!({"hooks": {"PostCompact": [theirs]}});
    std::fs::create_dir_all(it.settings().parent().unwrap()).unwrap();
    std::fs::write(it.settings(), serde_json::to_string_pretty(&kept).unwrap()).unwrap();
    assert_eq!(it.run(&["connect", "claude-code"]).0, Some(0));
    assert_eq!(it.groups("PostCompact"), [theirs.clone(), it.group()]);
    let (code, out) = it.run(&["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nPostCompact hook: removed ("), "{out}");
    assert_eq!(it.groups("PostCompact"), [theirs]);
}

/// Disconnect removes AtticDB's PostCompact group, and a rerun finds none.
#[test]
fn disconnect_removes_post_compact_hook() {
    let it = Compact::new();
    assert_eq!(it.run(&["connect", "claude-code"]).0, Some(0));
    let (code, out) = it.run(&["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nPostCompact hook: removed ("), "{out}");
    let read = || serde_json::from_slice::<Value>(&std::fs::read(it.settings()).unwrap()).unwrap();
    // The SessionStart array stays as `[]`, as it always has. The emptied PostCompact key goes.
    assert_eq!(read()["hooks"], json!({"SessionStart": []}));
    let (code, out) = it.run(&["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nPostCompact hook: not found ("), "{out}");
}

/// When removing the emptied PostCompact key leaves `hooks` empty, `hooks` goes too. A user's
/// own empty PostCompact array, which disconnect removed nothing from, stays.
#[test]
fn disconnect_drops_the_hooks_it_emptied() {
    let it = Compact::new();
    std::fs::create_dir_all(it.settings().parent().unwrap()).unwrap();
    std::fs::write(it.settings(), "{\n  \"model\": \"opus\"\n}\n").unwrap();
    assert_eq!(it.run(&["connect", "claude-code"]).0, Some(0));
    let read = || serde_json::from_slice::<Value>(&std::fs::read(it.settings()).unwrap()).unwrap();
    // The SessionStart group removed by hand, so PostCompact is all that is left in `hooks`.
    let mut root = read();
    root["hooks"].as_object_mut().unwrap().remove("SessionStart");
    std::fs::write(it.settings(), serde_json::to_string_pretty(&root).unwrap()).unwrap();
    let (code, out) = it.run(&["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nPostCompact hook: removed ("), "{out}");
    assert_eq!(read(), json!({"model": "opus", "permissions": {"allow": []}}));

    let theirs = "{\n  \"hooks\": {\n    \"PostCompact\": []\n  }\n}\n";
    std::fs::write(it.settings(), theirs).unwrap();
    let (code, out) = it.run(&["disconnect", "claude-code"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nPostCompact hook: not found ("), "{out}");
    assert_eq!(std::fs::read_to_string(it.settings()).unwrap(), theirs);
}

/// `--no-hook` adds neither the SessionStart group nor the PostCompact one.
#[test]
fn connect_no_hook_skips_post_compact() {
    let it = Compact::new();
    let (code, out) = it.run(&["connect", "claude-code", "--no-hook"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(!out.contains("PostCompact hook") && !out.contains("SessionStart hook"), "{out}");
    assert_eq!(it.groups("PostCompact"), Vec::<Value>::new());
    assert_eq!(it.groups("SessionStart"), Vec::<Value>::new());
}

/// A config.toml as people keep it: a few tables, `[features]`, another MCP server and a hook
/// approval.
const CODEX_CONFIG: &str = r#"model = "gpt-5.5"
approval_policy = "on-request"
sandbox_mode = "workspace-write"

[features]
web_search_request = true

[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env_vars = ["GITHUB_TOKEN"]

[projects."/Users/me/src/app"]
trust_level = "trusted"

[hooks.state."/Users/me/.codex/hooks.json:SessionStart:0:0"]
trusted_hash = "sha256:0f3e5a"
"#;

/// A hooks.json as people keep it: 2 SessionStart groups and a Stop group.
const CODEX_HOOKS: &str = r#"{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "~/.codex/hooks/branch.sh",
            "timeout": 10,
            "statusMessage": "Reading the branch"
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "cat ~/notes/today.md"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "say done"
          }
        ]
      }
    ]
  }
}
"#;

/// A global AGENTS.md as people keep it.
const CODEX_AGENTS: &str = "# Working agreements\n\n- Run `cargo test` before you commit.\n- Keep \
                            PRs small.\n";

/// `connect codex` puts the MCP block at the end of `config.toml`, checks it with
/// `codex mcp get atticdb --json`, appends the SessionStart hook to `hooks.json`, and appends
/// the guide to the `AGENTS.md` Codex reads, in `$CODEX_HOME` or `~/.codex`. A rerun writes
/// nothing and runs no codex. `disconnect` leaves hand-kept files byte for byte as they were,
/// CRLF ones too. `--notes ask` leaves out the `memory_append` approval, and `--no-hook` the
/// hook. Neither changes the guide. An AtticDB server outside the block
/// is never edited, even with `--force`. A failed read-back puts the file back. Malformed
/// markers skip the part. The other part still applies, and the exit code is 1.
#[test]
fn connect_codex_adds_its_parts() {
    let dir = Dir::new();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
    let exe = exe.to_str().unwrap().to_owned();
    let (bin, empty) = (dir.0.join("bin"), dir.0.join("empty"));
    let log = support::fake_codex(&bin);
    std::fs::create_dir(&empty).unwrap();
    let calls = || -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    };
    let get = ["mcp", "get", "atticdb", "--json"].map(String::from).to_vec();
    let run =
        |home: &Path, codex: Option<&Path>, path: &Path, env: &[(&str, &Path)], args: &[&str]| {
            let mut command = support::command(&exe);
            command.args(args).current_dir(&dir.0).env("HOME", home).env("PATH", path);
            command.env_remove("ATTICDB_TOKEN").envs(codex.map(|codex| ("CODEX_HOME", codex)));
            command.envs(env.iter().copied());
            let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
            (status.code(), out)
        };
    let toml = |text: &str| format!("\"{}\"", text.replace('\\', r"\\").replace('"', "\\\""));
    let block = |home: Option<&str>, approve: bool| {
        let mut lines = vec![
            "# atticdb:begin v1. atticdb disconnect codex removes this block.".to_owned(),
            "[mcp_servers.atticdb]".into(),
            format!("command = {}", toml(&exe)),
            r#"args = ["mcp", "--agent", "codex"]"#.into(),
            r#"default_tools_approval_mode = "writes""#.into(),
        ];
        lines.extend(home.map(|home| format!("env = {{ ATTICDB_HOME = {} }}", toml(home))));
        if approve {
            let table =
                ["", "[mcp_servers.atticdb.tools.memory_append]", "approval_mode = \"approve\""];
            lines.extend(table.map(String::from));
        }
        lines.push("# atticdb:end".into());
        lines.iter().map(|line| format!("{line}\n")).collect::<String>()
    };
    // The guide's block, from its spec: the MCP server's instructions between 2 markers.
    let guide = format!(
        "<!-- atticdb:begin v1. atticdb disconnect codex removes this block. -->\n## AtticDB \
         memory\n{}\n<!-- atticdb:end -->\n",
        atticdb::mcp::INSTRUCTIONS
    );
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    let command = format!("{quoted} hook session-start --harness codex");
    let group = |command: &str| {
        json!({"hooks": [{"type": "command", "command": command, "timeout": 5,
            "statusMessage": "Loading AtticDB memory", "additionalContextLimit": 24000}]})
    };
    let read =
        |path: &Path| serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap();
    let text = |path: &Path| std::fs::read_to_string(path).unwrap();
    let perms = |path: &Path| std::fs::metadata(path).unwrap().mode() & 0o777;
    let groups = |path: &Path| read(path)["hooks"]["SessionStart"].as_array().unwrap().clone();
    let approve =
        "Open Codex once and approve the atticdb hook; Codex runs a new hook only after that.";
    let position = "Codex keys hook approvals by position, so it may ask you to approve the \
                    SessionStart hooks after atticdb's again.";

    let crlf = CODEX_CONFIG.replace('\n', "\r\n");
    let crlf_agents = CODEX_AGENTS.replace('\n', "\r\n");
    let rows = [
        ("fresh", false, None, ""),
        ("CODEX_HOME", true, None, ""),
        ("hand-kept", true, Some(CODEX_CONFIG), CODEX_AGENTS),
        ("CRLF", true, Some(crlf.as_str()), crlf_agents.as_str()),
        ("no final newline", true, Some(CODEX_CONFIG.trim_end()), CODEX_AGENTS.trim_end()),
    ];
    for (row, (case, moved, kept, agents_kept)) in rows.into_iter().enumerate() {
        let home = dir.0.join(format!("h{row}"));
        let codex_dir = if moved { home.join("c") } else { home.join(".codex") };
        let (config, hooks) = (codex_dir.join("config.toml"), codex_dir.join("hooks.json"));
        let agents = codex_dir.join("AGENTS.md");
        if let Some(kept) = kept {
            std::fs::create_dir_all(&codex_dir).unwrap();
            std::fs::write(&config, kept).unwrap();
            std::fs::write(&hooks, CODEX_HOOKS).unwrap();
            std::fs::write(&agents, agents_kept).unwrap();
            chmod(&config, 0o644);
            chmod(&hooks, 0o644);
            chmod(&agents, 0o644);
        }
        let codex = moved.then_some(codex_dir.as_path());
        let connect =
            |args: &[&str]| run(&home, codex, &bin, &[], &[&["connect", "codex"], args].concat());
        let disconnect = || run(&home, codex, &bin, &[], &["disconnect", "codex"]);
        let original = kept.unwrap_or_default();
        let eol = if case == "CRLF" { "\r\n" } else { "\n" };
        // A file without a final line end gets one, and the block's first line says so.
        let added = "removes this block and the line end it added above.";
        let with = |original: &str, block: &str| match original {
            "" => block.to_owned(),
            original if original.ends_with('\n') => {
                format!("{original}{eol}{}", block.replace('\n', eol))
            }
            original => {
                format!("{original}\n\n{}", block.replacen("removes this block.", added, 1))
            }
        };
        let (at, shown) = (config.display().to_string(), hooks.display().to_string());
        let read_by_codex = agents.display().to_string();
        let start = calls().len();

        let (code, out) = connect(&[]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let lines: Vec<&str> = out.lines().collect();
        let backup = |line: &str, was: &str| match kept {
            Some(_) => {
                let backup = line.split(", backup: ").nth(1).unwrap().trim_end_matches(')');
                assert_eq!(std::fs::read_to_string(backup).unwrap(), was, "{case}: backup");
                format!(", backup: {backup}")
            }
            None => String::new(),
        };
        let saved = backup(lines[0], original);
        assert_eq!(lines[0], format!("MCP entry: added ({at}{saved})"), "{case}");
        let saved = backup(lines[1], CODEX_HOOKS);
        assert_eq!(lines[1], format!("SessionStart hook: added ({shown}{saved})"), "{case}");
        let saved = backup(lines[2], agents_kept);
        assert_eq!(lines[2], format!("atticdb guide: added ({read_by_codex}{saved})"), "{case}");
        assert_eq!(lines[3..], [approve], "{case}");
        assert_eq!(calls()[start..], *std::slice::from_ref(&get), "{case}: read back");
        assert_eq!(text(&config), with(original, &block(None, true)), "{case}");
        assert_eq!(text(&agents), with(agents_kept, &guide), "{case}: the guide");
        let mut wanted: Value = serde_json::from_str(kept.map_or("{}", |_| CODEX_HOOKS)).unwrap();
        let mut list = wanted["hooks"]["SessionStart"].as_array().cloned().unwrap_or_default();
        list.push(group(&command));
        wanted["hooks"]["SessionStart"] = json!(list);
        assert_eq!(read(&hooks), wanted, "{case}: appended last");
        let wanted_mode = if kept.is_some() { 0o644 } else { 0o600 };
        let modes = (perms(&config), perms(&hooks), perms(&agents));
        assert_eq!(modes, (wanted_mode, wanted_mode, wanted_mode), "{case}");
        if kept.is_none() {
            assert_eq!(perms(&codex_dir), 0o700, "{case}: a new config dir is private");
        }

        let now = || (files(&codex_dir), calls().len());
        let before = now();
        let (code, out) = connect(&[]);
        let unchanged = format!(
            "MCP entry: unchanged ({at})\nSessionStart hook: unchanged ({shown})\natticdb guide: \
             unchanged ({read_by_codex})\n"
        );
        assert_eq!((code, out), (Some(0), unchanged), "{case}");
        assert_eq!(now(), before, "{case}: nothing written, no backup, no codex");

        let (code, out) = connect(&["--notes", "ask"]);
        assert_eq!(code, Some(0), "{case}: {out}");
        assert!(out.starts_with(&format!("MCP entry: updated ({at}, backup: ")), "{case}: {out}");
        let hook = format!(
            "\nSessionStart hook: unchanged ({shown})\natticdb guide: unchanged ({read_by_codex})\n"
        );
        assert!(out.ends_with(&hook), "{case}: {out}");
        assert_eq!(text(&config), with(original, &block(None, false)), "{case}: --notes ask");
        assert_eq!(text(&agents), with(agents_kept, &guide), "{case}: --notes ask, same guide");
        assert_eq!(calls()[before.1..], *std::slice::from_ref(&get), "{case}: read back");

        let (code, out) = disconnect();
        assert_eq!(code, Some(0), "{case}: {out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with(&format!("MCP entry: removed ({at}, backup: ")), "{out}");
        let removed = format!("SessionStart hook: removed ({shown}, backup: ");
        assert!(lines[1].starts_with(&removed), "{case}: {out}");
        let removed = format!("atticdb guide: removed ({read_by_codex}, backup: ");
        assert!(lines[2].starts_with(&removed), "{case}: {out}");
        assert_eq!(lines.len(), 3, "{case}: no note, as no group followed atticdb's: {out}");
        let check = |when: &str| {
            assert_eq!(text(&config), original, "{case} {when}");
            assert_eq!(text(&agents), agents_kept, "{case} {when}: AGENTS.md");
            match kept {
                Some(_) => assert_eq!(text(&hooks), CODEX_HOOKS, "{case} {when}"),
                None => {
                    assert_eq!(read(&hooks), json!({"hooks": {"SessionStart": []}}), "{case}")
                }
            }
        };
        check("disconnect");

        let (code, out) = connect(&["--no-hook"]);
        assert_eq!(code, Some(0), "{case}: {out}");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("MCP entry: added (") && lines.len() == 2, "{out}");
        assert!(lines[1].starts_with("atticdb guide: added ("), "{case}: {out}");
        assert_eq!(text(&config), with(original, &block(None, true)), "{case}: --no-hook");
        assert_eq!(text(&agents), with(agents_kept, &guide), "{case}: --no-hook");
        assert_eq!(disconnect().0, Some(0), "{case}");
        check("--no-hook disconnect");
    }

    // The guide goes in the file Codex reads: AGENTS.override.md when it holds more than
    // whitespace, else AGENTS.md. Markers and mentions in fenced code don't count. A file that
    // mentions AtticDB is the user's own guide, kept as it is. An older block of AtticDB's is
    // updated in place. Malformed markers, near misses such as an indented marker, a symlink,
    // a fence that never closes, or a file the block would take past the 1 MiB read cap skip
    // the part. --dry-run decides the same and writes nothing. A rerun writes nothing.
    // disconnect gives each file back byte for byte, a bare CR at the end too, and AGENTS.md
    // too when Codex now reads the override.
    let marks = |text: &str| format!("{CODEX_AGENTS}\n{text}");
    let (begin, end) = (
        "<!-- atticdb:begin v1. atticdb disconnect codex removes this block. -->",
        "<!-- atticdb:end -->",
    );
    let old = guide.replace(atticdb::mcp::INSTRUCTIONS, "Old text.");
    let pairs =
        "its atticdb begin lines and lines <!-- atticdb:end --> don't pair up: each must come \
                 once, in that order";
    let big = format!("{CODEX_AGENTS}{}\n", "x".repeat(32 * 1024));
    let near = "it has a line that looks like an atticdb marker but isn't exactly one";
    let spaced = marks(&guide.replace(&format!("{end}\n"), &format!("{end}  \n")));
    // No LF at all, so a reconnect can't take the CR and AtticDB's LF for CRLF.
    let bare_cr = "User text\r".to_owned();
    let at_cap = format!("{CODEX_AGENTS}{}\n", "y".repeat((1 << 20) - CODEX_AGENTS.len() - 8));
    let over_cap = "would be over 1 MiB with atticdb's block, and atticdb reads at most 1 MiB";
    // (case, AGENTS.override.md, AGENTS.md, what connect does, AGENTS.md after disconnect)
    let rows = [
        ("override", Some("Answer in French.\n"), marks(&guide), "added", CODEX_AGENTS.to_owned()),
        ("empty override", Some(" \n"), CODEX_AGENTS.into(), "added", CODEX_AGENTS.into()),
        (
            "fenced markers",
            None,
            format!("{CODEX_AGENTS}\n```markdown\n{guide}Ask atticdb first.\n```\n"),
            "added",
            format!("{CODEX_AGENTS}\n```markdown\n{guide}Ask atticdb first.\n```\n"),
        ),
        (
            "old block",
            None,
            marks(&format!("{old}\n## Later\n")),
            "updated",
            format!("{CODEX_AGENTS}\n## Later\n"),
        ),
        (
            "own guide",
            None,
            marks("Search AtticDB first.\n"),
            "kept",
            marks("Search AtticDB first.\n"),
        ),
        (
            "end only",
            None,
            marks(&format!("{end}\n")),
            "it has a line <!-- atticdb:end --> without an atticdb begin line",
            marks(&format!("{end}\n")),
        ),
        (
            "begin only",
            None,
            marks(&format!("{begin}\n")),
            "it has an atticdb begin line without a line <!-- atticdb:end -->",
            marks(&format!("{begin}\n")),
        ),
        (
            "2 begins",
            None,
            marks(&format!("{begin}\n{guide}")),
            pairs,
            marks(&format!("{begin}\n{guide}")),
        ),
        ("symlink", None, CODEX_AGENTS.into(), "is a symlink", CODEX_AGENTS.into()),
        (
            "open fence",
            None,
            marks("```sh\ncargo test\n"),
            "has a code fence that never closes",
            marks("```sh\ncargo test\n"),
        ),
        ("big", None, big.clone(), "added", big),
        ("indented marker", None, marks(&format!("  {guide}")), near, marks(&format!("  {guide}"))),
        ("trailing spaces", None, spaced.clone(), near, spaced),
        ("bare CR", None, bare_cr.clone(), "added", bare_cr),
        ("near cap", None, at_cap.clone(), over_cap, at_cap),
        // Only spaces and tabs may follow a closing fence, so U+00A0 leaves it open.
        (
            "nbsp fence",
            None,
            marks(&format!("```markdown\n{guide}```\u{a0}\n")),
            "has a code fence that never closes",
            marks(&format!("```markdown\n{guide}```\u{a0}\n")),
        ),
    ];
    for (row, (case, over, kept, does, left)) in rows.into_iter().enumerate() {
        let home = dir.0.join(format!("a{row}"));
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let (overrides, agents) =
            (codex_dir.join("AGENTS.override.md"), codex_dir.join("AGENTS.md"));
        if let Some(over) = over {
            std::fs::write(&overrides, over).unwrap();
        }
        if case == "symlink" {
            std::fs::write(codex_dir.join("mine.md"), &kept).unwrap();
            std::os::unix::fs::symlink("mine.md", &agents).unwrap();
        } else {
            std::fs::write(&agents, &kept).unwrap();
        }
        let target = if case == "override" { &overrides } else { &agents };
        let shown = target.display();
        let original = std::fs::read_to_string(target).unwrap();
        let connect = |args: &[&str]| {
            run(&home, None, &bin, &[], &[&["connect", "codex", "--no-hook"], args].concat())
        };
        let guide_line = |out: &str| {
            let line = out
                .lines()
                .find(|line| line.contains("atticdb guide") || line.contains("user's guide"));
            line.unwrap_or_default().to_owned()
        };
        let skipped = format!("atticdb guide: skipped, {shown}");
        let (plan, said, code) = match does {
            "added" | "updated" => {
                let verb = if does == "added" { "add" } else { "update" };
                (
                    format!("{shown}: {verb} the atticdb guide"),
                    format!("atticdb guide: {does} ({shown}, backup: "),
                    0,
                )
            }
            "kept" => (
                format!("{shown}: keep the user's guide, which mentions atticdb"),
                format!("atticdb guide: kept, {shown} already mentions atticdb"),
                0,
            ),
            why if why == over_cap || why == "is a symlink" || why.starts_with("has a code") => {
                (format!("{skipped} {does}"), format!("{skipped} {does}"), 1)
            }
            why => (
                format!("{skipped}: {why}. Nothing changed."),
                format!("{skipped}: {why}. Nothing changed."),
                1,
            ),
        };
        let before = support::tree(&home);
        let (status, out) = connect(&["--dry-run"]);
        assert_eq!(status, Some(code), "{case}: plan: {out}");
        assert!(guide_line(&out).starts_with(&plan), "{case}: plan: {out}");
        assert_eq!(support::tree(&home), before, "{case}: the plan wrote nothing");

        let (status, out) = connect(&[]);
        assert_eq!(status, Some(code), "{case}: {out}");
        let line = guide_line(&out);
        assert!(line.starts_with(&said), "{case}: {out}");
        let too_big = " over 32 KiB. Codex may cut its instructions there";
        assert_eq!(out.contains(too_big), case == "big", "{case}: {out}");
        // A file without a final LF gets one, and the begin line says so.
        let added = "removes this block and the line end it added above.";
        let wanted = match does {
            "added" if !original.ends_with('\n') => {
                format!("{original}\n\n{}", guide.replacen("removes this block.", added, 1))
            }
            "added" => format!("{original}\n{guide}"),
            "updated" => original.replace(&old, &guide),
            _ => original.clone(),
        };
        assert_eq!(text(target), wanted, "{case}");
        if matches!(does, "added" | "updated") {
            let backup = line[said.len()..].trim_end_matches(')');
            assert_eq!(text(Path::new(backup)), original, "{case}: backup");
        }
        if case == "override" || case == "empty override" {
            let other = if case == "override" { &agents } else { &overrides };
            assert_eq!(
                text(other),
                if case == "override" { kept.clone() } else { " \n".into() },
                "{case}"
            );
        }
        if case == "symlink" {
            assert!(std::fs::symlink_metadata(&agents).unwrap().is_symlink(), "{case}");
            assert_eq!(text(&codex_dir.join("mine.md")), kept, "{case}");
        }

        let before = support::tree(&home);
        let (status, out) = connect(&[]);
        assert_eq!(status, Some(code), "{case}: again: {out}");
        if code == 0 && does != "kept" {
            assert_eq!(guide_line(&out), format!("atticdb guide: unchanged ({shown})"), "{case}");
            let (_, out) = connect(&["--dry-run"]);
            assert_eq!(guide_line(&out), format!("{shown}: atticdb guide unchanged"), "{case}");
        }
        assert_eq!(support::tree(&home), before, "{case}: a rerun writes nothing");

        let (status, out) = run(&home, None, &bin, &[], &["disconnect", "codex"]);
        let malformed =
            code == 1 && !matches!(case, "symlink" | "open fence" | "near cap" | "nbsp fence");
        assert_eq!(status, Some(i32::from(malformed)), "{case}: {out}");
        let lines: Vec<&str> = out.lines().filter(|line| line.contains("atticdb guide")).collect();
        let removed = |path: &Path| format!("atticdb guide: removed ({}, backup: ", path.display());
        match case {
            "override" => {
                assert!(lines.len() == 2 && lines[0].starts_with(&removed(&overrides)), "{out}");
                assert!(lines[1].starts_with(&removed(&agents)), "{case}: {out}");
                assert_eq!(text(&overrides), original, "{case}");
            }
            _ if matches!(does, "added" | "updated") => {
                assert!(lines.len() == 1 && lines[0].starts_with(&removed(&agents)), "{out}");
            }
            "symlink" => {
                let alone =
                    format!("atticdb guide: left {shown} alone, as it is a symlink. If it holds");
                assert!(lines.len() == 1 && lines[0].starts_with(&alone), "{case}: {out}");
            }
            _ if malformed => assert!(lines.len() == 1 && lines[0].starts_with(&said), "{out}"),
            _ => assert_eq!(lines, [format!("atticdb guide: not found ({shown})")], "{case}"),
        }
        assert_eq!(text(&agents), left, "{case}: disconnect");
    }

    // Under ATTICDB_HOME, the block passes it in env, as a TOML string with `\` and `"`
    // escaped, and the hook command sets it, quoted. The hook then reads that home.
    let home = dir.0.join("z");
    let zh = home.join("it's \"z\\h\"");
    let _holders = support::Holders(vec![zh.join("data")]);
    let zh_str = zh.to_str().unwrap();
    let (code, out) = run(&home, None, &bin, &[("ATTICDB_HOME", &zh)], &["connect", "codex"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("MCP entry: added ("), "the fake codex read it back: {out}");
    let (config, hooks) = (home.join(".codex/config.toml"), home.join(".codex/hooks.json"));
    assert_eq!(text(&config), block(Some(zh_str), true));
    let moved = &groups(&hooks)[0]["hooks"][0]["command"];
    let prefix = format!("ATTICDB_HOME='{}' ", zh_str.replace('\'', r"'\''"));
    assert_eq!(*moved, format!("{prefix}{command}"));
    let seeded = ["fact", "set", "--key", "user/style", "--content", "short sentences"];
    let (code, _, err) =
        support::atticdb(&home, &home, &seeded, &[("ATTICDB_HOME", zh_str)], b"", 0);
    assert_eq!(code, Some(0), "{err}");
    std::fs::write(home.join("stdin"), support::cwd(&home)).unwrap();
    let mut sh = support::command("sh");
    sh.args(["-c", &format!("{} < stdin", moved.as_str().unwrap())]);
    sh.current_dir(&home).env("HOME", &home).stdin(Stdio::null());
    let (status, out) = Process::start(&mut sh).wait();
    assert_eq!(status.code(), Some(0), "{out}");
    let out: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    let block_text = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(block_text.contains("\n- user/style: short sentences"), "{block_text}");
    assert_eq!(run(&home, None, &bin, &[], &["disconnect", "codex"]).0, Some(0));
    assert_eq!(text(&config), "");

    // An AtticDB server AtticDB didn't write is refused, with --force too, and its env never
    // shows. Keys count decoded, and a table name inside an array doesn't count. `mcp_servers`
    // set inline is refused too, as AtticDB can't tell what it holds. The hook still goes in.
    // disconnect leaves both, which counts as skipped.
    let home = dir.0.join("f");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let secret = format!("sk-{}", "s3cret".repeat(6));
    let server = (
        "has an atticdb MCP server that atticdb didn't write. atticdb doesn't edit it, even with \
         --force, and adding the block would break the file",
        "has an atticdb MCP server that atticdb didn't write. Remove it by hand to disconnect it",
    );
    let inline = (
        "sets mcp_servers as an inline table, so atticdb can't tell whether it holds atticdb, and \
         adding the block would break the file",
        "sets mcp_servers as an inline table, so atticdb can't tell whether it holds atticdb. If \
         it does, remove atticdb there by hand",
    );
    let theirs = [
        (
            format!(
                "model = \"o3\"\n  mcp_servers = {{ gh = {{ env = {{ K = \"{secret}\" }} }} }}\n"
            ),
            inline,
        ),
        (
            format!(
                "[mcp_servers.atticdb]\ncommand = \"npx\"\nenv = {{ API_KEY = \"{secret}\" }}\n"
            ),
            server,
        ),
        (
            format!(
                "[mcp_servers]\natticdb = {{ command = \"npx\", env = {{ KEY = \"{secret}\" }} }}\n"
            ),
            server,
        ),
        (format!("model = \"o3\"\nmcp_servers.atticdb.env.API_KEY = \"{secret}\"\n"), server),
        (format!("[mcp_servers.\"attic\\u0064b\"]\ncommand = \"{secret}\"\n"), server),
        (
            format!(
                "args = [\n  [\"a\"],\n  [ \"b\" ],\n]\n[mcp_servers.atticdb]\ncommand = \"{secret}\"\n"
            ),
            server,
        ),
    ];
    for (theirs, (refused, left)) in &theirs {
        let refused = format!(
            "MCP entry: skipped, {} {refused}. Nothing changed. Merge this block into it by \
             hand:\n{}",
            config.display(),
            block(None, true)
        );
        for force in [&[][..], &["--force"]] {
            std::fs::write(&config, theirs).unwrap();
            let start = calls().len();
            let (code, out) = run(&home, None, &bin, &[], &[&["connect", "codex"], force].concat());
            assert_eq!(code, Some(1), "{theirs} {force:?}: {out}");
            assert!(out.starts_with(&refused), "{theirs} {force:?}: {out}");
            assert!(out.contains("\nSessionStart hook: "), "{out}");
            assert!(!out.contains(&secret), "{out}");
            assert_eq!(text(&config), *theirs, "{force:?}: unchanged");
            assert_eq!(calls().len(), start, "no codex ran");
        }
        let (code, out) = run(&home, None, &bin, &[], &["disconnect", "codex"]);
        assert_eq!(code, Some(1), "{theirs}: {out}");
        let left = format!("MCP entry: skipped, {} {left}\n", config.display());
        assert!(out.starts_with(&left), "{theirs}: {out}");
        assert!(out.contains("\nSessionStart hook: removed ("), "{out}");
        assert!(!out.contains(&secret), "{out}");
        assert_eq!(text(&config), *theirs);
    }

    // TOML that only looks like AtticDB's: its markers and server inside a multi-line string, a
    // quoted key with a dot, a table name inside an array, and a comment that starts like a
    // marker. connect appends the block, and disconnect gives the file back.
    let home = dir.0.join("n");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let begin = "# atticdb:begin v1. atticdb disconnect codex removes this block.";
    let lookalikes = [
        format!(
            "developer_instructions = \"\"\"\nKeep these lines:\n{begin}\n[mcp_servers.atticdb]\n\
             # atticdb:end\n\"\"\"\n{CODEX_CONFIG}"
        ),
        format!("{CODEX_CONFIG}[mcp_servers.\"atticdb.proxy\"]\ncommand = \"npx\"\n"),
        format!("servers = [\n  [\"mcp_servers.atticdb\"],\n]\n{CODEX_CONFIG}"),
        format!("# atticdb:beginner\n{CODEX_CONFIG}"),
    ];
    for kept in &lookalikes {
        std::fs::write(&config, kept).unwrap();
        let (code, out) = run(&home, None, &bin, &[], &["connect", "codex", "--no-hook"]);
        assert_eq!(code, Some(0), "{kept}: {out}");
        assert!(out.starts_with("MCP entry: added ("), "{kept}: {out}");
        assert_eq!(text(&config), format!("{kept}\n{}", block(None, true)), "{kept}");
        let (code, out) = run(&home, None, &bin, &[], &["disconnect", "codex"]);
        assert_eq!(code, Some(0), "{kept}: {out}");
        assert_eq!(text(&config), *kept, "{kept}: the string stays");
    }

    // AtticDB's block from an old binary path is updated, with a backup: its own bytes go, and
    // a fresh block goes at the end, after the tables that followed it.
    let home = dir.0.join("o");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let old = block(None, true).replace(&toml(&exe), "\"/opt/old/atticdb\"");
    let after = "\n[profiles.fast]\nmodel = \"o4-mini\"\n";
    let kept = format!("{CODEX_CONFIG}\n{old}{after}");
    std::fs::write(&config, &kept).unwrap();
    let (code, out) = run(&home, None, &bin, &[], &["connect", "codex", "--no-hook"]);
    assert_eq!(code, Some(0), "{out}");
    let first = out.lines().next().unwrap();
    let prefix = format!("MCP entry: updated ({}, backup: ", config.display());
    assert!(first.starts_with(&prefix), "{out}");
    assert_eq!(text(Path::new(first[prefix.len()..].trim_end_matches(')'))), kept);
    assert_eq!(text(&config), format!("{CODEX_CONFIG}{after}\n{}", block(None, true)));

    // Codex writes config.toml with toml_edit, which keeps a comment at the end of the file
    // after every table. So a table Codex adds lands inside AtticDB's markers, after AtticDB's
    // tables, with a blank line before it. Those bytes aren't AtticDB's: disconnect keeps them,
    // and connect moves its block after them. Every result still parses as TOML.
    let valid = |path: &Path| {
        let mut python = support::command("python3");
        let load = "import sys, tomllib; tomllib.load(open(sys.argv[1], 'rb'))";
        python.args(["-c", load]).arg(path).stdin(Stdio::null());
        let (status, out) = Process::start(&mut python).wait();
        assert_eq!(status.code(), Some(0), "{}: {out}", path.display());
    };
    // The fixture's own github server is renamed, so Codex's github table is the only one.
    let base = CODEX_CONFIG.replace("[mcp_servers.github]", "[mcp_servers.linear]");
    let home = dir.0.join("t");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let projects = "[projects.\"/home/u/repo\"]\ntrust_level = \"trusted\"\n";
    let github = "\n[mcp_servers.github]\ncommand = \"npx\"\nargs = [\"-y\", \"server-github\"]\n";
    // AtticDB's block for `approve`, with `foreign` between its last table and its end line.
    let inside = |foreign: &str, approve: bool| {
        let ours = block(None, approve);
        let (body, end) = ours.split_at(ours.len() - "# atticdb:end\n".len());
        format!("{base}\n{body}{foreign}{end}")
    };
    let connect = |args: &[&str]| {
        run(&home, None, &bin, &[], &[&["connect", "codex", "--no-hook"], args].concat())
    };
    // connect --notes ask keeps the github server, and puts its new block at the end.
    std::fs::write(&config, inside(github, true)).unwrap();
    valid(&config);
    let (code, out) = connect(&["--notes", "ask"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("MCP entry: updated ("), "{out}");
    assert_eq!(text(&config), format!("{base}{github}\n{}", block(None, false)));
    valid(&config);
    // A projects table as toml_edit adds it, and one after a comment someone wrote under
    // AtticDB's last key. disconnect keeps them byte for byte, the blank lines and the comment
    // too. AtticDB's tables and markers go. connect moves the block after them, then a rerun
    // changes nothing.
    for foreign in [format!("\n{projects}"), format!("# keep me too\n\n{projects}")] {
        std::fs::write(&config, inside(&foreign, true)).unwrap();
        valid(&config);
        let (code, out) = run(&home, None, &bin, &[], &["disconnect", "codex"]);
        assert_eq!(code, Some(0), "{out}");
        assert!(out.starts_with("MCP entry: removed ("), "{out}");
        assert_eq!(text(&config), format!("{base}{foreign}"));
        valid(&config);
        std::fs::write(&config, inside(&foreign, true)).unwrap();
        let (code, out) = connect(&[]);
        assert_eq!(code, Some(0), "{out}");
        assert!(out.starts_with("MCP entry: updated ("), "{out}");
        let moved = format!("{base}{foreign}\n{}", block(None, true));
        assert_eq!(text(&config), moved);
        let (code, out) = connect(&[]);
        assert_eq!(code, Some(0), "{out}");
        assert!(out.starts_with("MCP entry: unchanged ("), "{out}");
        assert_eq!(text(&config), moved);
        assert_eq!(run(&home, None, &bin, &[], &["disconnect", "codex"]).0, Some(0));
        assert_eq!(text(&config), format!("{base}{foreign}"));
        valid(&config);
    }

    // mcp_servers.atticdb settings besides AtticDB's own lines: a table after the block, or one
    // before it. Changing only the block would leave part of a server, so both
    // verbs skip the part, change nothing, and say to clean up by hand. Exit 1.
    let home = dir.0.join("u");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let orphans = [
        format!(
            "{CODEX_CONFIG}\n{}\n[mcp_servers.atticdb.env]\nKEY = \"{secret}\"\n",
            block(None, true)
        ),
        format!(
            "{CODEX_CONFIG}[mcp_servers.atticdb.env]\nKEY = \"{secret}\"\n\n{}",
            block(None, true)
        ),
    ];
    let said = format!(
        "MCP entry: skipped, {} has mcp_servers.atticdb settings besides atticdb's own lines in \
         its block, and atticdb doesn't edit them. Nothing changed. Remove them by hand, then \
         re-run the command\n",
        config.display()
    );
    for kept in &orphans {
        std::fs::write(&config, kept).unwrap();
        valid(&config);
        for args in [&["connect", "codex", "--no-hook"][..], &["disconnect", "codex"]] {
            let start = calls().len();
            let (code, out) = run(&home, None, &bin, &[], args);
            assert_eq!(code, Some(1), "{args:?} {kept}: {out}");
            assert!(out.starts_with(&said) && !out.contains(&secret), "{args:?}: {out}");
            assert_eq!((text(&config), calls().len()), (kept.clone(), start), "{args:?}");
        }
    }

    // A removed AtticDB group with groups after it moves them up, so disconnect says Codex may
    // ask to approve them again, even when an AtticDB handler in a mixed group after it is left
    // to remove by hand.
    let home = dir.0.join("g");
    let hooks = home.join(".codex/hooks.json");
    std::fs::create_dir_all(hooks.parent().unwrap()).unwrap();
    let mixed = json!({"hooks": [{"type": "command", "command": "echo hi"},
        {"type": "command", "command": command, "timeout": 5}]});
    let other = json!({"hooks": [{"type": "command", "command": "cat notes.md"}]});
    let kept = json!({"hooks": {"SessionStart": [group(&command), &mixed, &other]}});
    std::fs::write(&hooks, serde_json::to_string_pretty(&kept).unwrap()).unwrap();
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "codex"]);
    assert_eq!(code, Some(1), "{out}");
    let left = "\nSessionStart hook: removed, but found another inside a group with other hooks; \
                remove that by hand there (";
    assert!(out.contains(left) && out.ends_with(&format!("\n{position}\n")), "{out}");
    assert_eq!(groups(&hooks), [mixed, other]);

    // Without codex on PATH, the block goes in, unverified, and that is not an error.
    let home = dir.0.join("m");
    let start = calls().len();
    let (code, out) = run(&home, None, &empty, &[], &["connect", "codex", "--no-hook"]);
    assert_eq!(code, Some(0), "{out}");
    let config = home.join(".codex/config.toml");
    let said = format!(
        "MCP entry: added ({}), not verified: codex is not on PATH\natticdb guide: added ({})\n",
        config.display(),
        home.join(".codex/AGENTS.md").display()
    );
    assert_eq!(out, said);
    assert_eq!((text(&config), calls().len()), (block(None, true), start));

    // A read-back that doesn't show AtticDB's entry, isn't JSON, or fails, puts the file back,
    // with its mode, and leaves no backup. One AtticDB made is deleted again. codex's output is
    // never shown, as its errors can quote the config. codex runs in the config dir. The hook
    // still goes in.
    let token = format!("tok-{}", "0123456789abcdef".repeat(2));
    let reads = [
        ("mismatch", "does not show "),
        ("garbage", "did not print JSON"),
        ("fails", "failed (exit 1); run it yourself to see why. "),
    ];
    for (mode, said) in reads {
        for kept in [Some(CODEX_CONFIG), None] {
            let home = dir.0.join(format!("b-{mode}-{}", kept.is_some()));
            let codex_dir = home.join(".codex");
            let config = codex_dir.join("config.toml");
            if let Some(kept) = kept {
                std::fs::create_dir_all(&codex_dir).unwrap();
                std::fs::write(&config, kept).unwrap();
                chmod(&config, 0o640);
            }
            let fake = [("FAKE_CODEX", Path::new(mode)), ("FAKE_CODEX_TOKEN", Path::new(&token))];
            let (code, out) = run(&home, None, &bin, &fake, &["connect", "codex"]);
            assert_eq!(code, Some(1), "{mode}: {out}");
            let failed = format!("MCP entry: failed, codex mcp get atticdb --json {said}");
            assert!(out.starts_with(&failed), "{mode}: {out}");
            assert!(!out.contains(&token) && !out.contains("api_key"), "{mode}: {out}");
            let cwd = std::fs::read_to_string(bin.join("cwd")).unwrap();
            let codex_dir_real = std::fs::canonicalize(&codex_dir).unwrap();
            assert_eq!(cwd.lines().last(), codex_dir_real.to_str(), "{mode}: codex's cwd");
            assert!(out.contains("\nSessionStart hook: added ("), "{mode}: {out}");
            let names: Vec<String> = files(&codex_dir).into_keys().collect();
            let left = if kept.is_some() {
                &["AGENTS.md", "config.toml", "hooks.json"][..]
            } else {
                &["AGENTS.md", "hooks.json"]
            };
            assert_eq!(names, left, "{mode}: no backup left");
            match kept {
                Some(kept) => {
                    let back = format!("{} is back as it was.", config.display());
                    assert!(out.contains(&back), "{mode}: {out}");
                    assert_eq!((text(&config), perms(&config)), (kept.to_owned(), 0o640));
                }
                None => {
                    assert!(out.contains(" is deleted again, as atticdb made it."), "{out}");
                    assert!(!config.exists(), "{mode}");
                }
            }
        }
    }

    // codex changes config.toml while AtticDB checks it, then shows another command. AtticDB
    // doesn't put the file back over that change: it keeps the backup, or a file it made, and
    // says how to restore by hand.
    let edit = "\n[projects.\"/edited\"]\ntrust_level = \"trusted\"\n";
    for kept in [Some(CODEX_CONFIG), None] {
        let home = dir.0.join(format!("e-{}", kept.is_some()));
        let codex_dir = home.join(".codex");
        let config = codex_dir.join("config.toml");
        if let Some(kept) = kept {
            std::fs::create_dir_all(&codex_dir).unwrap();
            std::fs::write(&config, kept).unwrap();
        }
        let fake = [("FAKE_CODEX", Path::new("edits"))];
        let (code, out) = run(&home, None, &bin, &fake, &["connect", "codex", "--no-hook"]);
        assert_eq!(code, Some(1), "{out}");
        assert!(out.starts_with("MCP entry: failed, codex mcp get atticdb --json does not show "));
        let changed = format!(
            "{} changed while atticdb was checking it, so atticdb didn't ",
            config.display()
        );
        assert!(out.contains(&changed), "{out}");
        let wrote = match kept {
            Some(kept) => format!("{kept}\n{}", block(None, true)),
            None => block(None, true),
        };
        assert_eq!(text(&config), format!("{wrote}{edit}"), "codex's edit stays");
        valid(&config);
        match kept {
            Some(kept) => {
                let backup = out.split("before atticdb wrote it is in ").nth(1).unwrap();
                let backup = backup.split(", to restore by hand").next().unwrap();
                assert_eq!(text(Path::new(backup)), kept, "the backup is kept");
            }
            None => {
                let advice = "and the [mcp_servers.atticdb] and [mcp_servers.atticdb.*] tables";
                assert!(out.contains(advice) && out.contains("keep anything else"), "{out}");
            }
        }
    }

    // Marker lines that don't pair up skip the part in both verbs, and change nothing.
    let home = dir.0.join("k");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let pairs = "its atticdb begin lines and lines # atticdb:end don't pair up: each must come \
                 once, in that order";
    let rows = [
        ("# atticdb:end\n".to_owned(), "it has a line # atticdb:end without an atticdb begin line"),
        (format!("{begin}\n"), "it has an atticdb begin line without a line # atticdb:end"),
        (format!("{begin}\n{begin}\n# atticdb:end\n"), pairs),
        (format!("# atticdb:end\n{begin}\n"), pairs),
    ];
    for (marks, said) in rows {
        let kept = format!("{CODEX_CONFIG}\n{marks}");
        std::fs::write(&config, &kept).unwrap();
        for verb in ["connect", "disconnect"] {
            let start = calls().len();
            let (code, out) = run(&home, None, &bin, &[], &[verb, "codex"]);
            assert_eq!(code, Some(1), "{verb} {marks}: {out}");
            let skipped =
                format!("MCP entry: skipped, {}: {said}. Nothing changed.", config.display());
            assert!(out.starts_with(&skipped), "{verb} {marks}: {out}");
            assert!(out.contains("\nSessionStart hook: "), "{verb}: {out}");
            assert_eq!((text(&config), calls().len()), (kept.clone(), start), "{verb} {marks}");
        }
    }

    // hooks = false in [features] warns, unless there is no hook.
    let home = dir.0.join("w");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let warning = format!("atticdb: {} sets hooks = false in [features]", config.display());
    for off in ["[features]\nhooks = false # for now\n", "features.hooks = false\n"] {
        for (flags, warns) in [(&[][..], true), (&["--no-hook"], false)] {
            std::fs::write(&config, off).unwrap();
            let (code, out) = run(&home, None, &bin, &[], &[&["connect", "codex"], flags].concat());
            assert_eq!(code, Some(0), "{off}: {out}");
            assert_eq!(out.contains(&warning), warns, "{off} {flags:?}: {out}");
            assert!(!out.contains("approve the atticdb hook"), "{off} {flags:?}: hooks are off");
        }
    }

    // Groups after AtticDB's move up when it goes, so Codex asks to approve them again. Both
    // verbs say so. An older AtticDB group is replaced by 1 at the end.
    let home = dir.0.join("p");
    let hooks = home.join(".codex/hooks.json");
    std::fs::create_dir_all(hooks.parent().unwrap()).unwrap();
    let kept: Value = serde_json::from_str(CODEX_HOOKS).unwrap();
    let theirs = kept["hooks"]["SessionStart"].as_array().unwrap().clone();
    let first = |ours: Value| {
        let mut hooks = kept.clone();
        let list = [&[ours][..], &theirs].concat();
        hooks["hooks"]["SessionStart"] = json!(list);
        serde_json::to_string_pretty(&hooks).unwrap()
    };
    std::fs::write(&hooks, first(group("'/opt/old/atticdb' hook session-start --harness codex")))
        .unwrap();
    let (code, out) = run(&home, None, &bin, &[], &["connect", "codex"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nSessionStart hook: updated ("), "{out}");
    assert!(out.ends_with(&format!("\n{approve}\n{position}\n")), "{out}");
    assert_eq!(groups(&hooks), [&theirs[..], &[group(&command)]].concat());
    std::fs::write(&hooks, first(group(&command))).unwrap();
    let (code, out) = run(&home, None, &bin, &[], &["disconnect", "codex"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains("\nSessionStart hook: removed (") && out.ends_with(&format!("\n{position}\n"))
    );
    assert_eq!(groups(&hooks), theirs);

    // A relative CODEX_HOME would point codex at another config. Both verbs refuse it before
    // any change, with exit 2.
    let home = dir.0.join("r");
    let start = calls().len();
    for verb in ["connect", "disconnect"] {
        let (code, out) = run(&home, Some(Path::new("rel")), &bin, &[], &[verb, "codex"]);
        assert_eq!(code, Some(2), "{verb}: {out}");
        assert!(out.contains("CODEX_HOME rel must be an absolute path"), "{verb}: {out}");
    }
    assert!(!home.exists() && !dir.0.join("rel").exists(), "nothing written");
    assert_eq!(calls().len(), start, "no codex ran");

    for args in [
        &["codex", "--notes", "maybe"][..],
        &["codex", "--bogus"],
        &["codex", "--no-hook", "--no-hook"],
    ] {
        let (code, out) = run(&dir.0, None, &empty, &[], &[&["connect"], args].concat());
        assert_eq!(code, Some(2), "{args:?}: {out}");
    }
    assert!(calls().iter().all(|call| *call == get), "only codex mcp get atticdb --json runs");
}

/// A home for the entry-only apps, Antigravity and Claude Desktop: an empty `HOME`, and a
/// `PATH` with no programs.
struct Plain {
    dir: Dir,
    exe: String,
}

impl Plain {
    fn new() -> Self {
        let dir = Dir::new();
        std::fs::create_dir_all(dir.0.join("bin")).unwrap();
        let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
        Self { dir, exe: exe.to_str().unwrap().to_owned() }
    }

    /// Runs `atticdb` with `args`, and returns its exit code and output.
    fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        let mut command = support::command(&self.exe);
        command.args(args).current_dir(&self.dir.0).env("HOME", self.dir.0.join("h"));
        command.env("PATH", self.dir.0.join("bin")).env_remove("ATTICDB_TOKEN");
        let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
        (status.code(), out)
    }

    /// The config file of `app` under `HOME`.
    fn config(&self, app: &str) -> std::path::PathBuf {
        let home = self.dir.0.join("h");
        match app {
            "antigravity" => home.join(".gemini/config/mcp_config.json"),
            _ => home.join("Library/Application Support/Claude/claude_desktop_config.json"),
        }
    }

    /// Writes `text` to `app`'s config, making its dir.
    fn write(&self, app: &str, text: &str) {
        let config = self.config(app);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(config, text).unwrap();
    }

    /// `app`'s config, read as JSON.
    fn read(&self, app: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.config(app)).unwrap()).unwrap()
    }

    /// The entry AtticDB writes for `app`.
    fn entry(&self, app: &str) -> Value {
        json!({"command": self.exe, "args": ["mcp", "--agent", app]})
    }
}

/// The entry-only apps on this OS, with their names in sentences. Claude Desktop has no Linux
/// app.
fn plain_apps() -> Vec<(&'static str, &'static str)> {
    let mut apps = vec![("antigravity", "Antigravity")];
    if cfg!(target_os = "macos") {
        apps.push(("claude-desktop", "Claude Desktop"));
    }
    apps
}

/// A config as people keep it: 1 user server, with an inline array, and another key.
const PLAIN_CONFIG: &str = r#"{
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"]
    }
  },
  "preferences": {
    "quickEntryShortcut": "off"
  }
}
"#;

/// Connect adds AtticDB's entry next to the user's server, and says to restart the app.
#[test]
fn connect_adds_entry_for_entry_only_apps() {
    let it = Plain::new();
    for (app, title) in plain_apps() {
        it.write(app, PLAIN_CONFIG);
        let (code, out) = it.run(&["connect", app]);
        assert_eq!(code, Some(0), "{app}: {out}");
        let shown = it.config(app).display().to_string();
        assert!(out.starts_with(&format!("MCP entry: added ({shown}, backup: ")), "{app}: {out}");
        assert!(out.ends_with(&format!(")\nRestart {title} to load it.\n")), "{app}: {out}");
        let root = it.read(app);
        assert_eq!(root["mcpServers"]["atticdb"], it.entry(app), "{app}");
        let theirs =
            json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"]});
        assert_eq!(root["mcpServers"]["github"], theirs, "{app}");
        assert_eq!(root["preferences"], json!({"quickEntryShortcut": "off"}), "{app}");
    }
    if !cfg!(target_os = "macos") {
        let (code, out) = it.run(&["connect", "claude-desktop"]);
        assert_eq!(code, Some(1), "{out}");
        assert!(out.contains("Claude Desktop is only available on macOS and Windows"), "{out}");
    }
}

/// Disconnect gives back the user's file byte for byte, its own empty `mcpServers` too. It
/// never removes `mcpServers`, so a file that had none keeps an empty one.
#[test]
fn connect_keeps_user_entries() {
    let it = Plain::new();
    let empty_servers = "{\n  \"mcpServers\": {}\n}\n";
    let no_servers = "{\n  \"preferences\": {\n    \"quickEntryShortcut\": \"off\"\n  }\n}\n";
    let kept_empty = "{\n  \"preferences\": {\n    \"quickEntryShortcut\": \"off\"\n  },\n  \
                      \"mcpServers\": {}\n}\n";
    let cases =
        [(PLAIN_CONFIG, PLAIN_CONFIG), (empty_servers, empty_servers), (no_servers, kept_empty)];
    for (app, _) in plain_apps() {
        for (text, after) in cases {
            it.write(app, text);
            let (code, out) = it.run(&["connect", app]);
            assert_eq!(code, Some(0), "{app}: {out}");
            assert_eq!(it.read(app)["mcpServers"]["atticdb"], it.entry(app), "{app}");
            let (code, out) = it.run(&["disconnect", app]);
            assert_eq!(code, Some(0), "{app}: {out}");
            let shown = it.config(app).display().to_string();
            assert!(out.starts_with(&format!("MCP entry: removed ({shown}, backup: ")), "{out}");
            assert_eq!(std::fs::read_to_string(it.config(app)).unwrap(), after, "{app}");
            let (code, out) = it.run(&["disconnect", app]);
            assert_eq!((code, out), (Some(0), format!("MCP entry: not found ({shown})\n")));
        }
    }
}

/// An older entry is updated where it stands: it stays first, and the user's server after it
/// keeps its bytes. So does one with flags, replaced with `--force`.
#[test]
fn connect_updates_entries_in_place() {
    let it = Plain::new();
    for (app, _) in plain_apps() {
        for (args, force) in
            [(r#"["mcp", "--agent", "APP"]"#, false), (r#"["mcp", "--url", "/x"]"#, true)]
        {
            let args = args.replace("APP", app);
            let config = format!(
                "{{\n  \"mcpServers\": {{\n    \"atticdb\": {{\n      \"args\": {args},\n      \
                 \"command\": \"/old/atticdb\"\n    }},\n    \"github\": {{\"command\": \"npx\", \
                 \"args\": [\"-y\"]}}\n  }}\n}}\n"
            );
            it.write(app, &config);
            let mut run = vec!["connect", app];
            run.extend(force.then_some("--force"));
            let (code, out) = it.run(&run);
            assert_eq!(code, Some(0), "{app}: {out}");
            assert!(out.starts_with("MCP entry: updated ("), "{app}: {out}");
            let block = format!(
                "    \"atticdb\": {{\n      \"args\": [\n        \"mcp\",\n        \"--agent\",\n        \
                 \"{app}\"\n      ],\n      \"command\": {}\n    }},\n",
                json!(it.exe)
            );
            let github = "    \"github\": {\"command\": \"npx\", \"args\": [\"-y\"]}\n  }\n}\n";
            let want = format!("{{\n  \"mcpServers\": {{\n{block}{github}");
            assert_eq!(std::fs::read_to_string(it.config(app)).unwrap(), want, "{app} {force}");
        }
    }
}

/// A rerun reports the entry unchanged, writes nothing, and asks for no restart.
#[test]
fn connect_is_idempotent_for_new_apps() {
    let it = Plain::new();
    for (app, _) in plain_apps() {
        it.write(app, PLAIN_CONFIG);
        assert_eq!(it.run(&["connect", app]).0, Some(0), "{app}");
        let parent = it.config(app).parent().unwrap().to_owned();
        let before = files(&parent);
        let shown = it.config(app).display().to_string();
        let (code, out) = it.run(&["connect", app]);
        assert_eq!((code, out), (Some(0), format!("MCP entry: unchanged ({shown})\n")), "{app}");
        let (code, out) = it.run(&["connect", app, "--dry-run"]);
        assert_eq!((code, out), (Some(0), format!("{shown}: MCP entry unchanged\n")), "{app}");
        assert_eq!(files(&parent), before, "{app}: nothing written");
    }
}

/// An entry from an older binary is updated. One with connection flags is kept unless
/// `--force`.
#[test]
fn connect_updates_stale_entries_for_new_apps() {
    let it = Plain::new();
    for (app, title) in plain_apps() {
        let mut stale = it.entry(app);
        stale["command"] = json!("/old/atticdb");
        it.write(app, &json!({"mcpServers": {"atticdb": stale}}).to_string());
        let (code, out) = it.run(&["connect", app]);
        assert_eq!(code, Some(0), "{app}: {out}");
        assert!(out.starts_with("MCP entry: updated (") && out.contains(title), "{app}: {out}");
        assert_eq!(it.read(app)["mcpServers"]["atticdb"], it.entry(app), "{app}");

        let flagged = json!({"command": it.exe, "args": ["mcp", "--url", "/old"]});
        it.write(app, &json!({"mcpServers": {"atticdb": flagged}}).to_string());
        let parent = it.config(app).parent().unwrap().to_owned();
        let before = files(&parent);
        let (code, out) = it.run(&["connect", app]);
        assert_eq!(code, Some(1), "{app}: {out}");
        let force = format!("atticdb connect {app} --force");
        assert!(out.contains("uses --url /old.") && out.contains(&force), "{app}: {out}");
        assert!(out.contains(&format!("quit {title}, then")), "{app}: {out}");
        assert_eq!(files(&parent), before, "{app}: nothing written");
        let (code, out) = it.run(&["connect", app, "--force"]);
        assert_eq!(code, Some(0), "{app}: {out}");
        assert_eq!(it.read(app)["mcpServers"]["atticdb"], it.entry(app), "{app}");
    }
}

/// A config with a comment or a trailing comma is never rewritten. The report shows the
/// entry to add by hand.
#[test]
fn connect_leaves_non_strict_json_alone() {
    let it = Plain::new();
    let texts = [
        "{\n  // my servers\n  \"mcpServers\": {}\n}\n",
        "{\n  \"mcpServers\": {\n    \"x\": {\"command\": \"x\"},\n  }\n}\n",
    ];
    for (app, title) in plain_apps() {
        for text in texts {
            it.write(app, text);
            let parent = it.config(app).parent().unwrap().to_owned();
            let before = files(&parent);
            let (code, out) = it.run(&["connect", app]);
            assert_eq!(code, Some(1), "{app}: {out}");
            let shown = it.config(app).display().to_string();
            assert!(out.starts_with(&format!("MCP entry: skipped, {shown}: ")), "{app}: {out}");
            let by_hand = format!("Add \"atticdb\": {} to \"mcpServers\" by hand", it.entry(app));
            assert!(out.contains(&by_hand), "{app}: {out}");
            assert!(!out.contains(&format!("Restart {title}")), "{app}: {out}");
            assert_eq!(files(&parent), before, "{app}: unchanged");
            let (code, out) = it.run(&["disconnect", app]);
            assert_eq!(code, Some(1), "{app}: {out}");
            assert!(out.contains("Remove \"atticdb\" from \"mcpServers\" by hand"), "{out}");
            assert_eq!(files(&parent), before, "{app}: unchanged by disconnect");
        }
    }
}

/// With the app's dir but no config, connect creates an owner-only config with the entry.
#[test]
fn connect_creates_missing_config() {
    let it = Plain::new();
    for (app, title) in plain_apps() {
        let config = it.config(app);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let (code, out) = it.run(&["connect", app]);
        assert_eq!(code, Some(0), "{app}: {out}");
        let shown = config.display();
        assert_eq!(out, format!("MCP entry: added ({shown})\nRestart {title} to load it.\n"));
        assert_eq!(it.read(app), json!({"mcpServers": {"atticdb": it.entry(app)}}), "{app}");
        assert_eq!(std::fs::metadata(&config).unwrap().mode() & 0o777, 0o600, "{app}");
    }
}

/// Under `ATTICDB_HOME`, the entry passes it on.
#[test]
fn connect_new_apps_pass_the_home() {
    let it = Plain::new();
    let moved = it.dir.0.join("moved");
    for (app, _) in plain_apps() {
        let mut command = support::command(&it.exe);
        command.args(["connect", app]).current_dir(&it.dir.0).env("HOME", it.dir.0.join("h"));
        command.env("PATH", it.dir.0.join("bin")).env("ATTICDB_HOME", &moved);
        let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
        assert_eq!(status.code(), Some(0), "{app}: {out}");
        let mut entry = it.entry(app);
        entry["env"] = json!({"ATTICDB_HOME": moved});
        assert_eq!(it.read(app)["mcpServers"]["atticdb"], entry, "{app}");
    }
}

/// A migrated Antigravity install: `~/.gemini/config/mcp_config.json`, and the legacy
/// `~/.gemini/antigravity/mcp_config.json` as a symlink to it. Connect and disconnect edit the
/// real file by its own path, and never write through the symlink.
#[test]
fn antigravity_edits_the_config_file_not_the_legacy_link() {
    let it = Plain::new();
    it.write("antigravity", PLAIN_CONFIG);
    let config = it.config("antigravity");
    let legacy_dir = it.dir.0.join("h/.gemini/antigravity");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let legacy = legacy_dir.join("mcp_config.json");
    let target = Path::new("../config/mcp_config.json");
    std::os::unix::fs::symlink(target, &legacy).unwrap();
    let shown = config.display().to_string();
    let (code, out) = it.run(&["connect", "antigravity", "--dry-run"]);
    assert_eq!((code, out), (Some(0), format!("{shown}: add the MCP entry\n")));
    let (code, out) = it.run(&["connect", "antigravity"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with(&format!("MCP entry: added ({shown}, backup: ")), "{out}");
    assert_eq!(it.read("antigravity")["mcpServers"]["atticdb"], it.entry("antigravity"));
    let meta = std::fs::symlink_metadata(&legacy).unwrap();
    assert!(meta.is_symlink(), "the legacy link stays a link");
    assert_eq!(std::fs::read_link(&legacy).unwrap(), target);
    assert!(std::fs::symlink_metadata(&config).unwrap().is_file(), "the config stays a file");
    let (code, out) = it.run(&["disconnect", "antigravity"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with(&format!("MCP entry: removed ({shown}, backup: ")), "{out}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), PLAIN_CONFIG);
    assert!(std::fs::symlink_metadata(&legacy).unwrap().is_symlink());
}

/// An older Antigravity install has only the legacy file, as a regular file. Connect edits it,
/// and makes no `~/.gemini/config`.
#[test]
fn antigravity_uses_a_regular_legacy_file() {
    let it = Plain::new();
    let legacy = it.dir.0.join("h/.gemini/antigravity/mcp_config.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, PLAIN_CONFIG).unwrap();
    let (code, out) = it.run(&["connect", "antigravity"]);
    assert_eq!(code, Some(0), "{out}");
    let shown = legacy.display().to_string();
    assert!(out.starts_with(&format!("MCP entry: added ({shown}, backup: ")), "{out}");
    let root: Value = serde_json::from_slice(&std::fs::read(&legacy).unwrap()).unwrap();
    assert_eq!(root["mcpServers"]["atticdb"], it.entry("antigravity"));
    assert!(!it.dir.0.join("h/.gemini/config").exists(), "no config dir made");
}

/// With neither file, but Antigravity's dir, connect creates `~/.gemini/config/mcp_config.json`
/// and writes nothing in the legacy dir.
#[test]
fn antigravity_creates_the_config_file() {
    let it = Plain::new();
    let legacy_dir = it.dir.0.join("h/.gemini/antigravity");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let (code, out) = it.run(&["connect", "antigravity"]);
    let shown = it.config("antigravity").display().to_string();
    assert_eq!(out, format!("MCP entry: added ({shown})\nRestart Antigravity to load it.\n"));
    assert_eq!(code, Some(0));
    let want = json!({"mcpServers": {"atticdb": it.entry("antigravity")}});
    assert_eq!(it.read("antigravity"), want);
    assert!(files(&legacy_dir).is_empty(), "nothing in the legacy dir");
}

/// A home for Gemini CLI: an empty `HOME` with `~/.gemini`, and a `PATH` with no programs.
struct Gemini {
    dir: Dir,
    exe: String,
}

impl Gemini {
    fn new() -> Self {
        let dir = Dir::new();
        std::fs::create_dir_all(dir.0.join("bin")).unwrap();
        std::fs::create_dir_all(dir.0.join("h/.gemini")).unwrap();
        let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_atticdb")).unwrap();
        Self { dir, exe: exe.to_str().unwrap().to_owned() }
    }

    /// Runs `atticdb` with `args` and `env`, and returns its exit code and output.
    fn run_with(&self, args: &[&str], env: &[(&str, &Path)]) -> (Option<i32>, String) {
        let mut command = support::command(&self.exe);
        command.args(args).current_dir(&self.dir.0).env("HOME", self.dir.0.join("h"));
        command.env("PATH", self.dir.0.join("bin")).env_remove("ATTICDB_TOKEN");
        command.envs(env.iter().copied());
        let (status, out) = Process::start(command.stdin(Stdio::null())).wait();
        (status.code(), out)
    }

    /// Runs `atticdb` with `args`, and returns its exit code and output.
    fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        self.run_with(args, &[])
    }

    fn settings(&self) -> std::path::PathBuf {
        self.dir.0.join("h/.gemini/settings.json")
    }

    fn guide(&self) -> std::path::PathBuf {
        self.dir.0.join("h/.gemini/GEMINI.md")
    }

    /// `settings.json`, read as JSON.
    fn read(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.settings()).unwrap()).unwrap()
    }

    /// The groups in `hooks.SessionStart` of `settings.json`.
    fn groups(&self) -> Vec<Value> {
        self.read()["hooks"]["SessionStart"].as_array().cloned().unwrap_or_default()
    }

    /// The entry AtticDB writes. `trust` unless notes ask.
    fn entry(&self, trust: bool) -> Value {
        let mut entry = json!({"command": self.exe, "args": ["mcp", "--agent", "gemini"]});
        if trust {
            entry["trust"] = json!(true);
        }
        entry
    }

    /// The SessionStart group AtticDB adds, with its timeout in milliseconds.
    fn group(&self, home: Option<&Path>) -> Value {
        let quoted = format!("'{}'", self.exe.replace('\'', r"'\''"));
        let prefix = home.map(|home| format!("ATTICDB_HOME='{}' ", home.display()));
        let command =
            format!("{}{quoted} hook session-start --harness gemini", prefix.unwrap_or_default());
        json!({"hooks": [{"type": "command", "name": "atticdb", "command": command,
            "timeout": 5000}]})
    }
}

/// The guide's block, from its spec: the MCP server's instructions between 2 markers that
/// name `atticdb disconnect gemini`.
fn gemini_block() -> String {
    format!(
        "<!-- atticdb:begin v1. atticdb disconnect gemini removes this block. -->\n## AtticDB \
         memory\n{}\n<!-- atticdb:end -->\n",
        atticdb::mcp::INSTRUCTIONS
    )
}

/// A settings.json as people keep it: a theme, 1 user server, and a user SessionStart group.
const GEMINI_SETTINGS: &str = r#"{
  "theme": "GitHub",
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"]
    }
  },
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [
          {
            "type": "command",
            "name": "branch",
            "command": "~/.gemini/hooks/branch.sh",
            "timeout": 2000
          }
        ]
      }
    ]
  }
}
"#;

/// A global GEMINI.md as people keep it.
const GEMINI_GUIDE: &str = "# My rules\n\n- Use pnpm, not npm.\n";

/// What `connect gemini` prints last in Apply mode.
const GEMINI_NEXT: &str = "Start a new Gemini CLI session to load atticdb.";

/// Connect adds the entry with `trust`, the SessionStart group and the guide block, next to
/// the user's own. A fresh `~/.gemini` gets both files. A rerun changes nothing, and a dry
/// run says what it would add.
#[test]
fn gemini_connect_adds_entry_hook_and_guide() {
    let it = Gemini::new();
    let (settings, guide) = (it.settings(), it.guide());
    let (at, shown) = (settings.display().to_string(), guide.display().to_string());
    let (code, out) = it.run(&["connect", "gemini", "--dry-run"]);
    let plan = format!(
        "{at}: add the MCP entry\n{at}: add the SessionStart hook\n{shown}: add the atticdb guide\n"
    );
    assert_eq!((code, out), (Some(0), plan));
    assert!(files(&it.dir.0.join("h/.gemini")).is_empty(), "the dry run wrote nothing");

    let (code, out) = it.run(&["connect", "gemini"]);
    let added = format!(
        "MCP entry: added ({at})\nSessionStart hook: added ({at})\natticdb guide: added \
         ({shown})\n{GEMINI_NEXT}\n"
    );
    assert_eq!((code, out), (Some(0), added));
    assert_eq!(
        it.read(),
        json!({"mcpServers": {"atticdb": it.entry(true)},
        "hooks": {"SessionStart": [it.group(None)]}})
    );
    assert_eq!(std::fs::read_to_string(&guide).unwrap(), gemini_block());

    let before = files(&it.dir.0.join("h/.gemini"));
    let (code, out) = it.run(&["connect", "gemini"]);
    let unchanged = format!(
        "MCP entry: unchanged ({at})\nSessionStart hook: unchanged ({at})\natticdb guide: \
         unchanged ({shown})\n{GEMINI_NEXT}\n"
    );
    assert_eq!((code, out), (Some(0), unchanged));
    assert_eq!(files(&it.dir.0.join("h/.gemini")), before, "nothing written");

    // Hand-kept files keep every byte of the user's, with AtticDB's parts added last.
    std::fs::write(&settings, GEMINI_SETTINGS).unwrap();
    std::fs::write(&guide, GEMINI_GUIDE).unwrap();
    let (code, out) = it.run(&["connect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with(&format!("MCP entry: added ({at}, backup: ")), "{out}");
    assert!(lines[1].starts_with(&format!("SessionStart hook: added ({at}, backup: ")), "{out}");
    assert!(lines[2].starts_with(&format!("atticdb guide: added ({shown}, backup: ")), "{out}");
    assert_eq!(lines[3..], [GEMINI_NEXT], "{out}");
    let root = it.read();
    assert_eq!(root["theme"], "GitHub");
    let github = json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"]});
    assert_eq!(root["mcpServers"], json!({"github": github, "atticdb": it.entry(true)}));
    let theirs: Value = serde_json::from_str(GEMINI_SETTINGS).unwrap();
    let theirs = theirs["hooks"]["SessionStart"][0].clone();
    assert_eq!(it.groups(), [theirs, it.group(None)]);
    let text = std::fs::read_to_string(&guide).unwrap();
    assert_eq!(text, format!("{GEMINI_GUIDE}\n{}", gemini_block()));
}

/// `--notes ask` leaves out `trust`, so Gemini CLI asks before each atticdb tool. Connecting
/// with the default notes puts it back, in place.
#[test]
fn gemini_notes_ask_drops_trust() {
    let it = Gemini::new();
    let (code, out) = it.run(&["connect", "gemini", "--notes", "ask"]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(it.read()["mcpServers"]["atticdb"], it.entry(false));
    let (code, out) = it.run(&["connect", "gemini", "--notes", "ask"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("MCP entry: unchanged ("), "{out}");
    let (code, out) = it.run(&["connect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("MCP entry: updated ("), "{out}");
    assert_eq!(it.read()["mcpServers"]["atticdb"], it.entry(true));
}

/// `--no-hook` adds no SessionStart group. The entry and the guide still go in.
#[test]
fn gemini_no_hook() {
    let it = Gemini::new();
    let (code, out) = it.run(&["connect", "gemini", "--no-hook"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(!out.contains("SessionStart hook"), "{out}");
    assert_eq!(it.read(), json!({"mcpServers": {"atticdb": it.entry(true)}}));
    assert_eq!(std::fs::read_to_string(it.guide()).unwrap(), gemini_block());
}

/// A user's own SessionStart group survives connect and disconnect, which give both files
/// back byte for byte.
#[test]
fn gemini_keeps_user_hook_groups() {
    let it = Gemini::new();
    std::fs::write(it.settings(), GEMINI_SETTINGS).unwrap();
    std::fs::write(it.guide(), GEMINI_GUIDE).unwrap();
    let (code, out) = it.run(&["connect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    let theirs: Value = serde_json::from_str(GEMINI_SETTINGS).unwrap();
    let theirs = theirs["hooks"]["SessionStart"][0].clone();
    assert_eq!(it.groups(), [theirs.clone(), it.group(None)]);
    let (code, out) = it.run(&["disconnect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nSessionStart hook: removed ("), "{out}");
    assert_eq!(std::fs::read_to_string(it.settings()).unwrap(), GEMINI_SETTINGS);
    assert_eq!(std::fs::read_to_string(it.guide()).unwrap(), GEMINI_GUIDE);
}

/// The user's group in [`GEMINI_HAND_WIRED`], as its bytes stand.
const GEMINI_USER_GROUP: &str = r#"      {
        "matcher": "startup",
        "hooks": [{"type": "command", "name": "branch", "command": "~/.gemini/hooks/branch.sh"}]
      }"#;

/// A settings.json wired by hand: an unquoted atticdb hook with another harness's flag,
/// before the user's own group.
fn gemini_hand_wired(command: &str) -> String {
    format!(
        "{{\n  \"hooks\": {{\n    \"SessionStart\": [\n      {{\n        \"hooks\": [\n          \
         {{\n            \"type\": \"command\",\n            \"command\": {}\n          }}\n        \
         ]\n      }},\n{GEMINI_USER_GROUP}\n    ]\n  }}\n}}\n",
        json!(command)
    )
}

/// A SessionStart group wired by hand to run an atticdb binary's `hook session-start` is
/// AtticDB's, quoted or not, whatever flags follow. Connect replaces it where it stands, so
/// Gemini CLI never gets 2 memory blocks. The user's group next to it keeps its bytes.
/// Disconnect then removes it. A command that runs another program stays the user's.
#[test]
fn gemini_replaces_a_hand_wired_hook_in_place() {
    let it = Gemini::new();
    let at = it.settings().display().to_string();
    let user: Value = serde_json::from_str(GEMINI_USER_GROUP).unwrap();
    let commands = [
        "/Users/sunny/.cargo/bin/atticdb hook session-start --harness claude-code",
        "'/opt/my tools/atticdb' hook session-start",
        "\"/usr/local/bin/atticdb.exe\" hook  session-start --harness gemini --home /h",
        "/opt/atticdb hook session-start --harness claude-code --home '/x y'",
    ];
    for command in commands {
        std::fs::write(it.settings(), gemini_hand_wired(command)).unwrap();
        let (code, out) = it.run(&["connect", "gemini", "--dry-run"]);
        assert_eq!(code, Some(0), "{command}: {out}");
        assert!(out.contains(&format!("\n{at}: update the SessionStart hook\n")), "{out}");
        let (code, out) = it.run(&["connect", "gemini"]);
        assert_eq!(code, Some(0), "{command}: {out}");
        let line = format!("\nSessionStart hook: updated ({at}, backup: ");
        assert!(out.contains(&line), "{command}: {out}");
        assert_eq!(it.groups(), [it.group(None), user.clone()], "{command}");
        let text = std::fs::read_to_string(it.settings()).unwrap();
        assert!(text.contains(&format!("}},\n{GEMINI_USER_GROUP}\n    ]")), "{text}");
        let (code, out) = it.run(&["connect", "gemini"]);
        assert_eq!(code, Some(0), "{command}: {out}");
        assert!(out.contains(&format!("\nSessionStart hook: unchanged ({at})\n")), "{out}");
        let (code, out) = it.run(&["disconnect", "gemini"]);
        assert_eq!(code, Some(0), "{command}: {out}");
        assert!(out.contains("\nSessionStart hook: removed ("), "{command}: {out}");
        assert_eq!(it.groups(), std::slice::from_ref(&user), "{command}");
    }

    // Disconnect alone removes a hand-wired group too.
    std::fs::write(it.settings(), gemini_hand_wired(commands[0])).unwrap();
    let (code, out) = it.run(&["disconnect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("\nSessionStart hook: removed ("), "{out}");
    assert_eq!(it.groups(), std::slice::from_ref(&user));

    // Not atticdb's: another binary, a relative path, another hook verb, or a command that
    // runs more than atticdb. The user's bytes stay, and AtticDB's group goes last.
    for command in [
        "/opt/bin/atticdb-wrapper hook session-start",
        "atticdb hook session-start --harness gemini",
        "/opt/bin/atticdb hook post-compact",
        "/opt/atticdb hook session-start && /opt/other-hook",
    ] {
        let text = gemini_hand_wired(command);
        std::fs::write(it.settings(), &text).unwrap();
        let theirs: Value = serde_json::from_str(&text).unwrap();
        let theirs = theirs["hooks"]["SessionStart"][0].clone();
        let (code, out) = it.run(&["connect", "gemini"]);
        assert_eq!(code, Some(0), "{command}: {out}");
        assert!(out.contains("\nSessionStart hook: added ("), "{command}: {out}");
        assert_eq!(it.groups(), [theirs, user.clone(), it.group(None)], "{command}");
        let kept = &text[..text.rfind("\n    ]").unwrap()];
        let after = std::fs::read_to_string(it.settings()).unwrap();
        assert!(after.starts_with(kept), "{command}: {after}");
        let (code, out) = it.run(&["disconnect", "gemini"]);
        assert_eq!(code, Some(0), "{command}: {out}");
        let after = std::fs::read_to_string(it.settings()).unwrap();
        assert!(after.starts_with(kept), "{command}: disconnect keeps it: {after}");
    }

    // A hand-wired atticdb handler in a group with another hook is left for the user, with
    // neutral words: it may not be older.
    let mixed = json!({"hooks": {"SessionStart": [{"hooks": [
        {"type": "command", "command": commands[0]},
        {"type": "command", "command": "/opt/other-hook"}
    ]}]}});
    let text = serde_json::to_string_pretty(&mixed).unwrap();
    std::fs::write(it.settings(), &text).unwrap();
    let (code, out) = it.run(&["connect", "gemini"]);
    assert_eq!(code, Some(1), "{out}");
    let said = format!(
        "SessionStart hook: skipped, {}: an atticdb hook is inside a group with other hooks. \
         Update the atticdb handler inside that group by hand, to run: ",
        it.settings().display()
    );
    assert!(out.contains(&said) && !out.contains("an older"), "{out}");
    assert_eq!(it.read()["hooks"], mixed["hooks"], "the group stays");
}

/// A GEMINI.md that mentions AtticDB outside a block is the user's own guide. It stays as it
/// is, and the guide part says so. The other parts still apply.
#[test]
fn gemini_guide_skips_user_guide() {
    let it = Gemini::new();
    let theirs = "# Memory\n\nUse AtticDB's memory_search before you ask me.\n";
    std::fs::write(it.guide(), theirs).unwrap();
    let shown = it.guide().display().to_string();
    let (code, out) = it.run(&["connect", "gemini", "--dry-run"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(&format!("\n{shown}: keep the user's guide, which mentions atticdb\n")));
    let (code, out) = it.run(&["connect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    let kept = format!("\natticdb guide: kept, {shown} already mentions atticdb\n");
    assert!(out.contains(&kept), "{out}");
    assert_eq!(it.read()["mcpServers"]["atticdb"], it.entry(true));
    assert_eq!(std::fs::read_to_string(it.guide()).unwrap(), theirs);
    let (code, out) = it.run(&["disconnect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.ends_with(&format!("\natticdb guide: not found ({shown})\n")), "{out}");
    assert_eq!(std::fs::read_to_string(it.guide()).unwrap(), theirs);
}

/// Disconnect removes the entry, the hook group and the guide block. A rerun finds none.
#[test]
fn gemini_disconnect_removes_all_parts() {
    let it = Gemini::new();
    assert_eq!(it.run(&["connect", "gemini"]).0, Some(0));
    let (at, shown) = (it.settings().display().to_string(), it.guide().display().to_string());
    let (code, out) = it.run(&["disconnect", "gemini"]);
    assert_eq!(code, Some(0), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with(&format!("MCP entry: removed ({at}, backup: ")), "{out}");
    assert!(lines[1].starts_with(&format!("SessionStart hook: removed ({at}, backup: ")), "{out}");
    assert!(lines[2].starts_with(&format!("atticdb guide: removed ({shown}, backup: ")), "{out}");
    assert_eq!(lines.len(), 3, "{out}");
    // The emptied `mcpServers` and SessionStart array stay, as for the other apps.
    assert_eq!(it.read(), json!({"mcpServers": {}, "hooks": {"SessionStart": []}}));
    assert_eq!(std::fs::read_to_string(it.guide()).unwrap(), "");
    let (code, out) = it.run(&["disconnect", "gemini"]);
    let none = format!(
        "MCP entry: not found ({at})\nSessionStart hook: not found ({at})\natticdb guide: not \
         found ({shown})\n"
    );
    assert_eq!((code, out), (Some(0), none));
}

/// A settings.json with a comment is never rewritten. Its parts say what to add by hand, the
/// guide still goes in, and the exit code is 1.
#[test]
fn gemini_leaves_non_strict_settings_alone() {
    let it = Gemini::new();
    let text = "{\n  // my theme\n  \"theme\": \"GitHub\"\n}\n";
    std::fs::write(it.settings(), text).unwrap();
    let (code, out) = it.run(&["connect", "gemini"]);
    assert_eq!(code, Some(1), "{out}");
    let entry = format!("Add \"atticdb\": {} to \"mcpServers\" by hand", it.entry(true));
    let group = format!("Add this group to hooks.SessionStart by hand: {}", it.group(None));
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("MCP entry: skipped, ") && lines[0].ends_with(&entry), "{out}");
    assert!(lines[1].starts_with("SessionStart hook: skipped, "), "{out}");
    assert!(lines[1].ends_with(&group), "{out}");
    assert!(lines[2].starts_with("atticdb guide: added ("), "{out}");
    assert_eq!(std::fs::read_to_string(it.settings()).unwrap(), text);
}

/// `GEMINI_CLI_HOME` moves Gemini CLI's config dir to its `.gemini`, as Gemini CLI reads it.
/// A relative one is refused. Under `ATTICDB_HOME`, the entry and the hook pass it on.
#[test]
fn gemini_follows_its_home_and_atticdb_home() {
    let it = Gemini::new();
    let (moved, home) = (it.dir.0.join("g"), it.dir.0.join("moved"));
    let env = [("GEMINI_CLI_HOME", moved.as_path()), ("ATTICDB_HOME", home.as_path())];
    let (code, out) = it.run_with(&["connect", "gemini"], &env);
    assert_eq!(code, Some(0), "{out}");
    let settings = moved.join(".gemini/settings.json");
    assert!(out.starts_with(&format!("MCP entry: added ({})\n", settings.display())), "{out}");
    let root: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    let mut entry = it.entry(true);
    entry["env"] = json!({"ATTICDB_HOME": home});
    assert_eq!(root["mcpServers"]["atticdb"], entry);
    assert_eq!(root["hooks"]["SessionStart"], json!([it.group(Some(&home))]));
    let guide = std::fs::read_to_string(moved.join(".gemini/GEMINI.md")).unwrap();
    assert_eq!(guide, gemini_block());
    assert!(files(&it.dir.0.join("h/.gemini")).is_empty(), "~/.gemini untouched");
    let (code, out) = it.run_with(&["disconnect", "gemini"], &env);
    assert_eq!(code, Some(0), "{out}");
    assert!(std::fs::read_to_string(moved.join(".gemini/GEMINI.md")).unwrap().is_empty());

    for verb in ["connect", "disconnect"] {
        let (code, out) = it.run_with(&[verb, "gemini"], &[("GEMINI_CLI_HOME", Path::new("g"))]);
        assert_eq!(code, Some(2), "{verb}: {out}");
        assert!(out.contains("GEMINI_CLI_HOME g must be an absolute path"), "{verb}: {out}");
    }
}
