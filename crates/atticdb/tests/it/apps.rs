//! The apps' parts on every platform. `atticdb setup` runs against a temp user profile, with a
//! fake `claude` on `PATH`. Then the hooks it wrote run as each app runs them. On Windows the
//! Copilot hook is a PowerShell line, and Claude Code's runs in exec form, with no shell.
//! Elsewhere both are `sh` lines. Nothing here reads or writes the real home.
//!
//! The binary runs from a copy in a folder whose name holds a space, `'`, `&` and `$`. So the
//! quoting of each hook is tested too.

use crate::portable::{drain, wait, Dir, Servers, COMMAND_TIMEOUT, HOME_VARS};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The fake `claude`, in Python. `mcp add -s user` and `mcp remove -s user <name>` edit
/// `.claude.json` in `CLAUDE_CONFIG_DIR`, else in the user's home, as Claude Code does.
const FAKE_CLAUDE: &str = r#"import json, os, sys
args = sys.argv[1:]
home = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~")
path = os.path.join(home, ".claude.json")
try:
    with open(path) as f:
        root = json.load(f)
except FileNotFoundError:
    root = {}
servers = root.setdefault("mcpServers", {})
if args[:4] == ["mcp", "add", "-s", "user"]:
    rest, env, words = args[4:], {}, []
    while rest:
        arg = rest.pop(0)
        if arg == "--":
            words += rest
            break
        if arg == "-e":
            while rest and not rest[0].startswith("-"):
                key, value = rest.pop(0).split("=", 1)
                env[key] = value
        else:
            words.append(arg)
    if len(words) < 2 or words[0] in servers:
        sys.exit("bad mcp add: " + json.dumps(args))
    servers[words[0]] = {"type": "stdio", "command": words[1], "args": words[2:], "env": env}
elif args[:4] == ["mcp", "remove", "-s", "user"] and len(args) == 5:
    if servers.pop(args[4], None) is None:
        sys.exit("no server " + args[4])
else:
    sys.exit("unexpected args: " + json.dumps(args))
with open(path, "w") as f:
    json.dump(root, f, indent=2)
"#;

/// The rule the hooks must show.
const RULE: &str = "- user/style: short sentences";

/// A temp user profile, with a copy of `atticdb` in a folder of its own and a fake `claude`.
struct Profile {
    root: PathBuf,
    /// The folder of fake programs, which is all of `PATH`, with System32 on Windows.
    bin: PathBuf,
    /// The copy of `atticdb` that every command runs.
    exe: PathBuf,
}

impl Profile {
    /// A profile at `root`, with `atticdb` copied into `root/<folder>`.
    fn new(root: &Path, folder: &str) -> Self {
        let bin = root.join("fakes");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = root.join(folder).join(if cfg!(windows) { "atticdb.exe" } else { "atticdb" });
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_atticdb"), &exe).unwrap();
        Self { root: root.to_owned(), bin, exe }
    }

    /// Adds the fake `claude`, which runs [`FAKE_CLAUDE`] with Python.
    fn fake_claude(&self) {
        let python = python();
        std::fs::write(self.bin.join("claude.py"), FAKE_CLAUDE).unwrap();
        if cfg!(windows) {
            // As npm installs it: a `.cmd` shim.
            let shim = format!("@\"{}\" \"%~dp0claude.py\" %*\r\n", python.display());
            std::fs::write(self.bin.join("claude.cmd"), shim).unwrap();
        } else {
            let script = format!("#!/bin/sh\nexec '{}' \"$0.py\" \"$@\"\n", python.display());
            let claude = self.bin.join("claude");
            std::fs::write(&claude, script).unwrap();
            #[cfg(unix)]
            crate::support::chmod(&claude, 0o755);
        }
    }

    /// Gives `command` this profile as the user's, and [`Self::bin`] as `PATH`. It inherits
    /// none of [`HOME_VARS`].
    fn env<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        for var in HOME_VARS {
            command.env_remove(var);
        }
        command.env_remove("ATTICDB_SETUP_INTERACTIVE");
        if cfg!(windows) {
            command.env("USERPROFILE", &self.root);
            command.env("APPDATA", self.root.join("AppData").join("Roaming"));
            command.env("LOCALAPPDATA", self.root.join("AppData").join("Local"));
            let path = std::env::join_paths([self.bin.clone(), system32()]).unwrap();
            command.env("PATH", path)
        } else {
            command.env("HOME", &self.root).env("PATH", &self.bin)
        }
    }

    /// Runs the copy of `atticdb` with `args` in `cwd`, with `env` added.
    fn atticdb(&self, cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> Output {
        let mut command = Command::new(&self.exe);
        self.env(&mut command).args(args).current_dir(cwd);
        for (name, value) in env {
            command.env(name, value);
        }
        output(&mut command, b"")
    }

    /// VS Code's user `mcp.json`.
    fn vscode_file(&self) -> PathBuf {
        let user = if cfg!(windows) {
            self.root.join("AppData").join("Roaming")
        } else if cfg!(target_os = "macos") {
            self.root.join("Library/Application Support")
        } else {
            self.root.join(".config")
        };
        user.join("Code").join("User").join("mcp.json")
    }

    /// The default home's data directory.
    fn default_data(&self) -> PathBuf {
        let base = if cfg!(windows) {
            self.root.join("AppData").join("Local")
        } else if cfg!(target_os = "macos") {
            self.root.join("Library/Application Support")
        } else {
            self.root.join(".local/share")
        };
        base.join("atticdb").join("data")
    }
}

/// `%SystemRoot%\System32`.
fn system32() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(root).join("System32")
}

/// Python on the test's `PATH`, not the Windows Store alias.
fn python() -> PathBuf {
    let name = if cfg!(windows) { "python.exe" } else { "python3" };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path).filter(|dir| !dir.ends_with("WindowsApps"));
    let found = dirs.map(|dir| dir.join(name)).find(|python| python.is_file());
    found.unwrap_or_else(|| panic!("the fake claude needs {name} on PATH"))
}

/// The exit code, stdout and stderr of a finished command.
type Output = (Option<i32>, String, String);

/// Spawns `command`, retrying while Linux says a program just copied is busy. Another test
/// thread may fork while the copy's file is open.
fn spawn(command: &mut Command) -> Child {
    for _ in 1..50 {
        match command.spawn() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(20));
            }
            spawned => return spawned.unwrap(),
        }
    }
    command.spawn().unwrap()
}

/// Runs `command` with `stdin` written and then closed, for up to [`COMMAND_TIMEOUT`].
fn output(command: &mut Command, stdin: &[u8]) -> Output {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn(command);
    let (out, err) = (drain(child.stdout.take().unwrap()), drain(child.stderr.take().unwrap()));
    let mut input = child.stdin.take().unwrap();
    let _ = input.write_all(stdin);
    drop(input);
    let status = wait(&mut child, COMMAND_TIMEOUT);
    let out = out.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let err = err.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    assert!(status.is_some(), "{command:?} did not exit in time: {out}{err}");
    (status.and_then(|status| status.code()), out, err)
}

fn read_json(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap()
}

/// The PowerShells that may run Copilot's `powershell` line: Windows PowerShell, and
/// PowerShell 7 when it is on `PATH`.
#[cfg(windows)]
fn powershells() -> Vec<PathBuf> {
    let mut found = vec![system32().join(r"WindowsPowerShell\v1.0\powershell.exe")];
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pwsh = std::env::split_paths(&path).map(|dir| dir.join("pwsh.exe")).find(|p| p.is_file());
    found.extend(pwsh);
    found
}

/// Commands that run Copilot's hook `entry` as Copilot does: its `powershell` line on
/// Windows, else its `bash` line, in `profile` with the entry's `env`.
fn copilot_hook(profile: &Profile, entry: &Value) -> Vec<Command> {
    #[cfg(windows)]
    let commands = powershells().into_iter().map(|shell| {
        let mut command = Command::new(shell);
        command.args(["-NoProfile", "-NonInteractive", "-Command"]);
        command.arg(entry["powershell"].as_str().unwrap());
        command
    });
    #[cfg(not(windows))]
    let commands = std::iter::once({
        let mut command = Command::new("/bin/sh");
        command.args(["-c", entry["bash"].as_str().unwrap()]);
        command
    });
    let env = entry["env"].as_object().cloned().unwrap_or_default();
    let with_env = |mut command: Command| {
        profile.env(&mut command);
        for (name, value) in &env {
            command.env(name, value.as_str().unwrap());
        }
        command
    };
    commands.map(with_env).collect()
}

/// A command that runs Claude Code's hook `handler` as Claude Code does: in exec form on
/// Windows, else its `command` in `sh`.
fn claude_hook(handler: &Value) -> Command {
    match handler.get("args") {
        Some(args) => {
            let mut command = Command::new(handler["command"].as_str().unwrap());
            command.args(args.as_array().unwrap().iter().map(|arg| arg.as_str().unwrap()));
            command
        }
        None => {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", handler["command"].as_str().unwrap()]);
            command
        }
    }
}

/// Connects VS Code, Copilot CLI and Claude Code on the default home, or on `moved` in the
/// profile, and checks every file. Then each hook, run as its app runs it with no
/// `ATTICDB_HOME`, prints the home's rule. Doctor passes, and Claude's MCP entry answers.
fn wire_and_load(moved: Option<&str>) {
    let dir = Dir::new();
    let profile = Profile::new(&dir.0.join("p"), "bin dir it's & $x");
    profile.fake_claude();
    let repo = profile.root.join("app");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let home = moved.map(|name| profile.root.join(name));
    // A hook that missed the moved home would start the default one, so both are stopped.
    let moved_data = home.iter().map(|home| home.join("data"));
    let _servers = Servers(moved_data.chain([profile.default_data()]).collect());
    let env: Vec<(&str, &Path)> =
        home.iter().map(|home| ("ATTICDB_HOME", home.as_path())).collect();
    let apps = "vscode,copilot,claude-code";
    let (code, out, err) = profile.atticdb(&repo, &env, &["setup", "--yes", "--harness", apps]);
    assert_eq!(code, Some(0), "{out}{err}");
    for ok in ["Copilot CLI: ok", "VS Code: ok", "Claude Code: ok"] {
        assert!(out.contains(ok), "{ok}: {out}{err}");
    }

    // VS Code's entry names the copy, and the others name the same path.
    let home_env = home.as_ref().map(|home| json!({"ATTICDB_HOME": home}));
    let vscode = read_json(&profile.vscode_file());
    let entry = &vscode["servers"]["atticdb"];
    let exe = entry["command"].as_str().unwrap().to_owned();
    assert!(exe.contains("bin dir it's & $x"), "{vscode}");
    assert_eq!(std::fs::canonicalize(&exe).unwrap(), std::fs::canonicalize(&profile.exe).unwrap());
    assert_eq!(entry["args"], json!(["mcp", "--agent", "vscode-copilot"]), "{vscode}");
    assert_eq!(entry.get("env"), home_env.as_ref(), "{vscode}");
    let copilot = read_json(&profile.root.join(".copilot").join("mcp-config.json"));
    assert_eq!(copilot["mcpServers"]["atticdb"]["command"], exe.as_str(), "{copilot}");

    let hook_file = read_json(&profile.root.join(".copilot").join("hooks").join("atticdb.json"));
    let hook = &hook_file["hooks"]["sessionStart"][0];
    let (shell, other) =
        if cfg!(windows) { ("powershell", "bash") } else { ("bash", "powershell") };
    assert!(hook[shell].is_string() && hook.get(other).is_none(), "{hook_file}");
    assert_eq!(hook["timeoutSec"], 5, "{hook_file}");
    assert_eq!(hook.get("env"), home_env.as_ref(), "{hook_file}");
    let guide = profile.root.join(".copilot").join("instructions").join("atticdb.instructions.md");
    assert!(std::fs::read_to_string(guide).unwrap().contains("memory_search"));

    let state = read_json(&profile.root.join(".claude.json"));
    let claude = &state["mcpServers"]["atticdb"];
    assert_eq!(claude["command"], exe.as_str(), "{state}");
    assert_eq!(claude["args"], json!(["mcp", "--agent", "claude-code"]), "{state}");
    let settings = read_json(&profile.root.join(".claude").join("settings.json"));
    let handler = &settings["hooks"]["SessionStart"][0]["hooks"][0];
    if cfg!(windows) {
        let mut args = vec!["hook", "session-start", "--harness", "claude-code"];
        let home = home.as_ref().map(|home| home.to_str().unwrap());
        args.extend(home.map(|home| ["--home", home]).into_iter().flatten());
        let wanted = json!({"type": "command", "command": exe, "args": args, "timeout": 5});
        assert_eq!(handler, &wanted, "{settings}");
    } else {
        let command = handler["command"].as_str().unwrap();
        assert!(command.ends_with(" hook session-start --harness claude-code"), "{settings}");
        assert_eq!(command.starts_with("ATTICDB_HOME="), home.is_some(), "{settings}");
    }

    let set = ["fact", "set", "--key", "user/style", "--content", "short sentences"];
    let (code, out, err) = profile.atticdb(&repo, &env, &set);
    assert_eq!(code, Some(0), "{out}{err}");

    // The files name the home, so the hooks run without ATTICDB_HOME.
    let stdin = json!({"cwd": repo}).to_string();
    for mut command in copilot_hook(&profile, hook) {
        let (code, out, err) = output(command.current_dir(&repo), stdin.as_bytes());
        assert_eq!(code, Some(0), "{command:?}: {out}{err}");
        let printed: Value = serde_json::from_str(out.trim()).unwrap_or_default();
        let block = printed["additionalContext"].as_str().unwrap_or_default();
        assert!(block.contains(RULE), "{command:?}: {out}{err}");
    }
    let stdin = json!({"cwd": repo, "hook_event_name": "SessionStart"}).to_string();
    let mut command = claude_hook(handler);
    let (code, out, err) = output(profile.env(&mut command).current_dir(&repo), stdin.as_bytes());
    assert_eq!(code, Some(0), "{out}{err}");
    let printed: Value = serde_json::from_str(out.trim()).unwrap_or_default();
    let block = printed["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default();
    assert!(block.contains(RULE), "{out}{err}");

    let (code, out, err) = profile.atticdb(&repo, &env, &["doctor", "--harness", apps]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("Server and store: ok"), "{out}{err}");

    // Claude's MCP entry, run as Claude runs it, answers a tool call from the home.
    let mut command = Command::new(claude["command"].as_str().unwrap());
    command.args(claude["args"].as_array().unwrap().iter().map(|arg| arg.as_str().unwrap()));
    profile.env(&mut command).current_dir(&repo);
    for (name, value) in claude["env"].as_object().cloned().unwrap_or_default() {
        command.env(name, value.as_str().unwrap());
    }
    let params = json!({"protocolVersion": "2025-11-25", "capabilities": {},
                        "clientInfo": {"name": "test", "version": "1"}});
    let get = json!({"name": "fact_get", "arguments": {"key": "user/style"}});
    let lines = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": get}),
    ];
    let stdin: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let (code, out, err) = output(&mut command, stdin.as_bytes());
    assert_eq!(code, Some(0), "{out}{err}");
    let replies: Vec<Value> = out.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let got = replies.iter().find(|reply| reply["id"] == 2).unwrap_or_else(|| panic!("{out}"));
    assert!(got.to_string().contains("short sentences"), "{got}");
}

#[test]
fn setup_wires_the_apps_and_their_hooks_load_the_default_home() {
    wire_and_load(None);
}

#[test]
fn setup_wires_the_apps_and_their_hooks_load_a_moved_home() {
    wire_and_load(Some("moved home"));
}

/// Codex gets its MCP entry everywhere. On Windows that is all, and setup and doctor say so in
/// 1 line. Elsewhere it also gets its hook and its guide.
#[test]
fn codex_gets_only_its_mcp_entry_on_windows() {
    let dir = Dir::new();
    let profile = Profile::new(&dir.0.join("p"), "bin");
    let _servers = Servers(vec![profile.default_data()]);
    let line = "Codex on Windows: MCP only, hooks wait for #137";
    let (code, out, err) = profile.atticdb(&dir.0, &[], &["setup", "--yes", "--harness", "codex"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(out.matches(line).count(), usize::from(cfg!(windows)), "{out}");
    let codex = profile.root.join(".codex");
    let config = std::fs::read_to_string(codex.join("config.toml")).unwrap();
    assert!(config.contains("[mcp_servers.atticdb]"), "{config}");
    for file in ["hooks.json", "AGENTS.md"] {
        assert_eq!(codex.join(file).exists(), cfg!(not(windows)), "{file}: {out}");
    }
    let (code, out, err) = profile.atticdb(&dir.0, &[], &["doctor", "--harness", "codex"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(out.matches(line).count(), usize::from(cfg!(windows)), "{out}");
}

/// A relative `--home` prints `{}`, as any hook error does, and exits 0. `stats` says why it
/// can't run on Windows.
#[test]
fn hook_home_must_be_absolute_and_stats_needs_a_unix_socket() {
    let dir = Dir::new();
    let profile = Profile::new(&dir.0.join("p"), "bin");
    let mut command = Command::new(&profile.exe);
    profile.env(&mut command).current_dir(&dir.0);
    command.args(["hook", "session-start", "--harness", "claude-code", "--home", "rel"]);
    let (code, out, err) = output(&mut command, json!({"cwd": dir.0}).to_string().as_bytes());
    assert_eq!((code, out.as_str()), (Some(0), "{}\n"), "{err}");
    assert!(err.contains("--home must be an absolute path"), "{err}");
    assert!(!dir.0.join("rel").exists() && !profile.root.join("rel").exists());
    for args in [&["stats", "--url", "http://127.0.0.1:1"][..], &["stats", "--data-dir", "d"]] {
        let (code, out, err) = profile.atticdb(&dir.0, &[], args);
        if cfg!(windows) {
            assert_eq!(code, Some(1), "{args:?}: {out}{err}");
            assert!(err.contains("Unix socket only, which Windows lacks"), "{args:?}: {err}");
        } else if args[1] == "--url" {
            assert_eq!(code, Some(2), "{args:?}: {out}{err}");
            assert!(err.contains("Unix socket only"), "{args:?}: {err}");
        }
    }
}

/// PowerShell reads `‘`, `’`, `‚` and `‛` as single quotes too. A binary in a folder named
/// with them still runs from the Copilot hook. The first run starts the server, which must
/// not hold the hook's output open, or Copilot would wait for it. Then the hook prints the
/// rule in each PowerShell.
#[cfg(windows)]
#[test]
fn the_powershell_hook_quotes_every_single_quote() {
    let dir = Dir::new();
    let profile = Profile::new(&dir.0.join("p"), "it\u{2019}s \u{2018}q\u{2019} \u{201a}\u{201b}");
    let data = profile.default_data();
    let _servers = Servers(vec![data.clone()]);
    let (code, out, err) = profile.atticdb(&dir.0, &[], &["connect", "copilot"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let set = ["fact", "set", "--key", "user/style", "--content", "short sentences"];
    let (code, out, err) = profile.atticdb(&dir.0, &[], &set);
    assert_eq!(code, Some(0), "{out}{err}");
    crate::portable::stop_servers(&[data]);
    let hook_file = read_json(&profile.root.join(".copilot").join("hooks").join("atticdb.json"));
    let hook = &hook_file["hooks"]["sessionStart"][0];
    std::fs::create_dir_all(dir.0.join("app")).unwrap();
    let stdin = json!({"cwd": dir.0.join("app")}).to_string();
    // A cold start may outlast the hook's budget, so then it prints {}. Its line must still
    // arrive, which it can't if the server holds the pipe.
    let mut first = copilot_hook(&profile, hook).remove(0);
    let (code, out, err) = output(&mut first, stdin.as_bytes());
    assert_eq!(code, Some(0), "{first:?}: {out}{err}");
    assert!(serde_json::from_str::<Value>(out.trim()).is_ok(), "{first:?}: {out}{err}");
    assert!(err.contains("atticdb: started server pid "), "{first:?}: {out}{err}");
    let get = ["fact", "get", "--key", "user/style"];
    let (code, out, err) = profile.atticdb(&dir.0, &[], &get);
    assert_eq!(code, Some(0), "{out}{err}");
    for mut command in copilot_hook(&profile, hook) {
        let (code, out, err) = output(&mut command, stdin.as_bytes());
        assert_eq!(code, Some(0), "{command:?}: {out}{err}");
        assert!(out.contains(RULE), "{command:?}: {hook_file}: {out}{err}");
    }
}
