//! Claude Code's 3 parts:
//!
//! - The MCP entry. Claude keeps it in `.claude.json` and rewrites that file itself, so AtticDB
//!   only reads it, and changes the entry through `claude mcp add` and `claude mcp remove`.
//!   It never runs `claude mcp get`, which starts the server to check it.
//! - The SessionStart hook, a group in `hooks.SessionStart` of `settings.json`.
//! - The allow rules in `permissions.allow` of `settings.json`: the read tools, and
//!   `memory_append` unless notes ask. Never `fact_set`, so saving a rule always asks.
//!
//! People edit `settings.json` by hand, so [`crate::jsonedit`] edits it and changes only the
//! bytes it must. Each part is found by its content, so `disconnect` also removes an
//! identical part added by hand.

use super::{
    absolute_env, add_or_update, binary, entry_exe, entry_home, home_words, moved_home, planned,
    read, refuse_flags, save, write, Harness, Line, Mode, Report, State, COPILOT_SHARED,
};
use crate::home::{user_home, Lookup, HOME_ENV};
use crate::jsonedit;
#[cfg(not(unix))]
use crate::platform::DirBuilderExt;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Whether Claude Code or Codex asks before AtticDB saves a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notes {
    /// The harness asks before each `memory_append`.
    Ask,
    /// `memory_append` is allowed, so notes are saved without asking.
    Auto,
}

/// The read tools Claude Code may call without asking.
const READS: [&str; 4] = [
    "mcp__atticdb__memory_search",
    "mcp__atticdb__memory_recent",
    "mcp__atticdb__fact_get",
    "mcp__atticdb__fact_list",
];
/// The rule that saves notes without asking.
const APPEND: &str = "mcp__atticdb__memory_append";
/// Where the hook groups go in `settings.json`, and in Codex's `hooks.json`.
const HOOKS: [&str; 2] = ["hooks", "SessionStart"];
/// Where the allow rules go.
const ALLOW: [&str; 2] = ["permissions", "allow"];
/// The `claude` args that remove AtticDB's entry.
const REMOVE: [&str; 5] = ["mcp", "remove", "-s", "user", "atticdb"];
/// The most of `.claude.json` read. It holds all of Claude's state, so it can be large.
const MAX_STATE: u64 = 64 << 20;
/// The most of a child's stdout read back.
const MAX_STDOUT: usize = 1 << 20;
/// How long a `claude` or `codex` command may run.
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

/// The variable that moves Claude Code's config dir.
const CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Refuses a relative `CLAUDE_CONFIG_DIR`. The `claude` child inherits it, so it would write
/// `.claude.json` under the current directory, where AtticDB doesn't look. An empty one
/// counts as unset.
fn check_config_env() -> Result<(), String> {
    match std::env::var_os(CONFIG_ENV) {
        Some(dir) if !dir.is_empty() && !Path::new(&dir).is_absolute() => Err(format!(
            "{CONFIG_ENV} {} must be an absolute path. Nothing changed",
            Path::new(&dir).display()
        )),
        _ => Ok(()),
    }
}

/// `$CLAUDE_CONFIG_DIR` when set and absolute, else `~/.claude`.
pub(super) fn config_dir() -> Result<PathBuf, String> {
    absolute_env(CONFIG_ENV).map_or_else(|| Ok(user_home()?.join(".claude")), Ok)
}

/// Claude's state file, which holds the user MCP servers: `.claude.json` in
/// `$CLAUDE_CONFIG_DIR` when set and absolute, else in `~`.
pub(crate) fn state_file() -> Result<PathBuf, String> {
    let dir = absolute_env(CONFIG_ENV).map_or_else(user_home, Ok)?;
    Ok(dir.join(".claude.json"))
}

/// Adds the MCP entry, the hook when `hook`, and the allow rules for `notes`. An entry that
/// differs is refused before anything changes, unless `force`. It stops with exit code 2 for
/// a relative `CLAUDE_CONFIG_DIR`. In Plan mode, it writes nothing and runs no `claude`.
pub(super) fn connect(
    force: bool,
    hook: bool,
    notes: Notes,
    mode: Mode,
    lookup: &Lookup,
) -> Report {
    if let Err(error) = check_config_env() {
        return Report::stopped(2, error);
    }
    let prepared = (|| {
        let home = moved_home(Harness::ClaudeCode, lookup)?;
        let exe = binary()?;
        let state = state_file()?;
        let current = current(&state);
        if let (false, Ok(Some(entry))) = (force, &current) {
            if !same(entry, &exe, home.as_deref()) && !updatable(entry) {
                let data = lookup.dir()?.join("data");
                refuse_flags(Harness::ClaudeCode, &state, entry, &data)?;
                return Err(differs(&state, entry, &exe, home.as_deref()));
            }
        }
        let warnings = split(Harness::ClaudeCode, home.as_deref());
        match mode {
            Mode::Apply => drop(lookup.ensure()?),
            Mode::Plan => lookup.check_existing()?,
        }
        Ok((home, exe, state, current, warnings))
    })();
    let (home, exe, state, current, warnings) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Report::stopped(1, error),
    };
    let home = home.as_deref();
    let entry = match current {
        Ok(current) => set_entry(&state, current.as_ref(), &exe, home, mode),
        Err(why) => Line::new(
            State::Skipped,
            format!(
                "MCP entry: skipped, {}: {why}. Run by hand: {}",
                state.display(),
                shell_line("claude", &add_args(&exe, home))
            ),
        ),
    };
    let group = hook_group(&exe, home);
    let add_group = |text: &str| add_hook(Harness::ClaudeCode, text, &group);
    let add_allow = |text: &str| add_rules(text, notes);
    let mut parts = Vec::new();
    if hook {
        let by_hand = format!("Add this group to hooks.SessionStart by hand: {group}");
        parts.push(Part { name: "SessionStart hook", by_hand, edit: &add_group });
    }
    let by_hand = format!("Add these to permissions.allow by hand: {}", json!(rules(notes)));
    parts.push(Part { name: "allow rules", by_hand, edit: &add_allow });
    let settings = edit_file(settings_file(), "{\n}\n", &parts, mode);
    // The hook is the first part, when it was asked for.
    let hook_state = settings.first().filter(|_| hook).map(|line| line.state);
    let mut lines = vec![entry];
    lines.extend(settings);
    let next = match mode {
        Mode::Apply => vec!["Start a new Claude Code session to load atticdb.".into()],
        Mode::Plan => Vec::new(),
    };
    Report { lines, warnings, next, hook: hook_state, ..Report::default() }
}

/// Removes the MCP entry when it is what AtticDB writes, every hook group that runs AtticDB's
/// hook, and AtticDB's allow rules. Returns the exit code: 2 for a relative
/// `CLAUDE_CONFIG_DIR`.
pub(super) fn disconnect() -> u8 {
    if let Err(error) = check_config_env() {
        eprintln!("atticdb: {error}");
        return 2;
    }
    let state = match state_file() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("atticdb: {error}");
            return 1;
        }
    };
    let mut every = READS.to_vec();
    every.push(APPEND);
    let drop_group = |text: &str| drop_hooks(Harness::ClaudeCode, text);
    let parts = [
        Part {
            name: "SessionStart hook",
            by_hand: format!(
                "Remove the groups that run atticdb{} from hooks.SessionStart by hand",
                hook_args(Harness::ClaudeCode)
            ),
            edit: &drop_group,
        },
        Part {
            name: "allow rules",
            by_hand: format!("Remove these from permissions.allow by hand: {}", json!(every)),
            edit: &drop_rules,
        },
    ];
    let mut lines = vec![drop_entry(&state)];
    lines.extend(edit_file(settings_file(), "{\n}\n", &parts, Mode::Apply));
    Report { lines, ..Report::default() }.print()
}

/// Notes on the other harnesses that pass a home other than `home`. They share no file with
/// `harness`, so connecting goes ahead, but memory splits between the homes.
pub(super) fn split(harness: Harness, home: Option<&str>) -> Vec<String> {
    let ours = home_words(home);
    let other = |other: Harness| {
        let theirs = match entry_home(other, &other.file().ok()?) {
            Ok(None) => return None,
            Ok(Some(theirs)) if theirs.as_deref().map(Path::new) == home.map(Path::new) => {
                return None
            }
            Ok(Some(theirs)) => home_words(theirs.as_deref()),
            Err(why) => format!("an unknown home, as {why}"),
        };
        Some(format!(
            "{} uses {theirs}, but {} now uses {ours}. Memory splits: each harness reads and \
             saves only its own home. Connect both under the same {HOME_ENV} to share it",
            other.name(),
            harness.name()
        ))
    };
    COPILOT_SHARED.into_iter().filter_map(other).collect()
}

/// The args AtticDB's entry passes.
fn mcp_args() -> Value {
    json!(["mcp", "--agent", Harness::ClaudeCode.agent()])
}

/// The `ATTICDB_HOME` an entry passes. An empty one counts as none.
fn home_of(entry: &Value) -> Option<&str> {
    entry["env"][HOME_ENV].as_str().filter(|home| !home.is_empty())
}

/// Whether `entry` sets no env but `ATTICDB_HOME`. Another variable, such as `HOME` or
/// `XDG_DATA_HOME`, could pick another home than the hook reads.
fn only_home_env(entry: &Value) -> bool {
    let env = entry.get("env");
    env.is_none_or(|env| env.as_object().is_some_and(|env| env.keys().all(|key| key == HOME_ENV)))
}

/// Whether `entry` runs `exe` with AtticDB's args on `home`, and sets nothing else in its env.
fn same(entry: &Value, exe: &str, home: Option<&str>) -> bool {
    entry["command"] == exe
        && entry["args"] == mcp_args()
        && home_of(entry).map(Path::new) == home.map(Path::new)
        && only_home_env(entry)
}

/// Whether `entry` runs any absolute path with exactly AtticDB's args, so it has no
/// connection flags. `connect` updates it without `--force`, as for VS Code and Copilot.
fn updatable(entry: &Value) -> bool {
    let command = entry["command"].as_str().map(Path::new);
    command.is_some_and(Path::is_absolute) && entry["args"] == mcp_args()
}

/// Whether `entry` is what AtticDB writes, for any absolute binary path and any home. The
/// binary's file name doesn't count, as an install may name it `atticdb-0.1.0`.
fn written(entry: &Value) -> bool {
    let Some(entry) = entry.as_object() else { return false };
    let command = entry.get("command").and_then(Value::as_str).map(Path::new);
    let atticdb = command.is_some_and(Path::is_absolute);
    let known = entry.keys().all(|key| ["type", "command", "args", "env"].contains(&key.as_str()));
    let stdio = entry.get("type").is_none_or(|kind| kind == "stdio");
    let env = only_home_env(&Value::Object(entry.clone()));
    atticdb && known && stdio && env && entry.get("args") == Some(&mcp_args())
}

/// Why `entry`, the entry in `state`, is not replaced without `--force`. It shows the
/// command, args and home, never the rest of its env.
fn differs(state: &Path, entry: &Value, exe: &str, home: Option<&str>) -> String {
    let shown = state.display();
    let (command, args) = (&entry["command"], &entry["args"]);
    format!(
        "{shown}: the atticdb entry for claude-code runs {command} {args} on {}, not an atticdb \
         mcp command such as {} {} on {}. Nothing changed. To replace it: atticdb connect \
         claude-code --force",
        home_words(home_of(entry)),
        json!(exe),
        mcp_args(),
        home_words(home)
    )
}

/// AtticDB's entry in the state file at `path`, `None` when there is none. A symlink or
/// anything but a regular file is refused, and so is a file over 64 MiB.
fn current(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read: {error}")),
        Ok(meta) if meta.is_symlink() => return Err("is a symlink".into()),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    crate::config::open_regular(OpenOptions::new().read(true), path)
        .and_then(|file| file.take(MAX_STATE + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot read: {error}"))?;
    if bytes.len() as u64 > MAX_STATE {
        return Err("is larger than 64 MiB".into());
    }
    let root: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("is not plain JSON ({error})"))?;
    Ok(root.get("mcpServers").and_then(|servers| servers.get("atticdb")).cloned())
}

/// The `claude` args that add AtticDB's entry for `exe`, passing `home` when set. `-e` takes
/// a list of values, up to `--` or the next option, so it goes after the name.
fn add_args(exe: &str, home: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["mcp", "add", "-s", "user", "atticdb"].map(String::from).to_vec();
    if let Some(home) = home {
        args.extend(["-e".into(), format!("{HOME_ENV}={home}")]);
    }
    let agent = Harness::ClaudeCode.agent();
    args.extend(["--", exe, "mcp", "--agent", agent].map(String::from));
    args
}

/// Makes AtticDB's entry run `exe` on `home`, through `claude`. `current` is the entry read
/// before. One that differs, such as one from an old binary path, is removed first. When
/// that removal worked but the add failed, the line shows the old entry, to restore it. In
/// Plan mode, `claude` never runs.
fn set_entry(
    state: &Path,
    current: Option<&Value>,
    exe: &str,
    home: Option<&str>,
    mode: Mode,
) -> Line {
    let shown = state.display();
    if current.is_some_and(|current| same(current, exe, home)) {
        return match mode {
            Mode::Plan => planned(state, "MCP entry", State::Unchanged),
            Mode::Apply => Line::new(State::Unchanged, format!("MCP entry: unchanged ({shown})")),
        };
    }
    let (add, remove) = (add_args(exe, home), REMOVE.map(String::from).to_vec());
    let by_hand = match current {
        Some(_) => format!("{} && {}", shell_line("claude", &remove), shell_line("claude", &add)),
        None => shell_line("claude", &add),
    };
    let Some(claude) = find_program("claude") else {
        let why = format!("MCP entry: skipped, claude is not on PATH. Run: {by_hand}");
        return Line::new(State::Skipped, why);
    };
    if mode == Mode::Plan {
        return planned(state, "MCP entry", add_or_update(current.is_some()));
    }
    let failed = |why: String| {
        Line::new(State::Failed, format!("MCP entry: failed, {why}. Run by hand: {by_hand}"))
    };
    if let Some(old) = current {
        if let Err(why) = run_claude(&claude, &remove) {
            return failed(why);
        }
        if let Err(why) = run_claude(&claude, &add) {
            let why = format!(
                "MCP entry: failed, {why}. The old entry was removed. {}. Re-run atticdb connect \
                 claude-code, which adds atticdb's now that no entry is there",
                keep_old(old)
            );
            return Line::new(State::Failed, why);
        }
    } else if let Err(why) = run_claude(&claude, &add) {
        return failed(why);
    }
    let (verb, done) = match current {
        Some(_) => ("updated", State::Updated),
        None => ("added", State::Added),
    };
    let again = "Re-run atticdb connect claude-code";
    let (state, text) = match read_back(state, |entry| entry.is_some_and(|e| same(e, exe, home))) {
        Ok(Some(entry)) if same(&entry, exe, home) => {
            (done, format!("MCP entry: {verb} ({shown})"))
        }
        Ok(_) => (
            State::Failed,
            format!("MCP entry: failed, {shown} does not show the entry after claude ran. {again}"),
        ),
        Err(why) => {
            (State::Failed, format!("MCP entry: failed, {shown} {why} after claude ran. {again}"))
        }
    };
    Line::new(state, text)
}

/// Saves `old`, an entry `claude` removed, to a new owner-only file in the config dir, as
/// its env may hold secrets. Returns what to print: the file's path, and the entry's command,
/// args and env names, never env values.
fn keep_old(old: &Value) -> String {
    let names: Vec<&str> = old["env"]
        .as_object()
        .map_or_else(Vec::new, |env| env.keys().map(String::as_str).collect());
    let shown =
        format!("It ran {} {} with env names {}", old["command"], old["args"], json!(names));
    let saved = config_dir().and_then(|dir| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|error| format!("{}: cannot create: {error}", dir.display()))?;
        save(&dir.join("atticdb-mcp-entry.json"), old.to_string().as_bytes(), 0o600)
    });
    match saved {
        Ok(path) => format!("{shown}. The whole entry is saved in {}", path.display()),
        Err(why) => format!("{shown}. It could not be saved: {why}"),
    }
}

/// AtticDB's entry, read again after `claude` ran, until `done` holds for it: up to 5 reads,
/// 50 ms apart. A running Claude Code may be rewriting the file.
fn read_back(state: &Path, done: impl Fn(Option<&Value>) -> bool) -> Result<Option<Value>, String> {
    let mut entry = current(state);
    for _ in 1..5 {
        if entry.as_ref().is_ok_and(|entry| done(entry.as_ref())) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        entry = current(state);
    }
    entry
}

/// Removes AtticDB's entry through `claude` when it is what AtticDB writes.
fn drop_entry(state: &Path) -> Line {
    let shown = state.display();
    let by_hand = shell_line("claude", &REMOVE.map(String::from));
    let skipped = |why: String| Line::new(State::Skipped, why);
    let entry = match current(state) {
        Ok(entry) => entry,
        Err(why) => {
            return skipped(format!("MCP entry: skipped, {shown} {why}. Run by hand: {by_hand}"))
        }
    };
    let Some(entry) = entry else {
        return Line::new(State::Unchanged, format!("MCP entry: not found ({shown})"));
    };
    if !written(&entry) {
        return skipped(format!(
            "MCP entry: skipped, it differs from the entry atticdb writes ({shown}). To remove it \
             anyway, run: {by_hand}"
        ));
    }
    let Some(claude) = find_program("claude") else {
        return skipped(format!("MCP entry: skipped, claude is not on PATH. Run: {by_hand}"));
    };
    if let Err(why) = run_claude(&claude, &REMOVE.map(String::from)) {
        let why = format!("MCP entry: failed, {why}. Run by hand: {by_hand}");
        return Line::new(State::Failed, why);
    }
    let again = "Re-run atticdb disconnect claude-code";
    let (state, text) = match read_back(state, |entry| entry.is_none()) {
        Ok(None) => (State::Removed, format!("MCP entry: removed ({shown})")),
        Ok(Some(_)) => (
            State::Failed,
            format!("MCP entry: failed, {shown} still shows the entry after claude ran. {again}"),
        ),
        Err(why) => {
            (State::Failed, format!("MCP entry: failed, {shown} {why} after claude ran. {again}"))
        }
    };
    Line::new(state, text)
}

/// The first `name` on `PATH` that is an executable regular file. Relative `PATH` entries
/// are skipped.
#[cfg(unix)]
pub(crate) fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs = std::env::split_paths(&path).filter(|dir| dir.is_absolute());
    dirs.map(|dir| dir.join(name)).find(|program| {
        std::fs::metadata(program)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

/// The first program called `name` on `PATH`, by [`find_in`].
// TODO(#137): part 3 checks how Claude Code and Codex install on Windows.
#[cfg(windows)]
pub(crate) fn find_program(name: &str) -> Option<PathBuf> {
    find_in(name, &std::env::var_os("PATH")?)
}

/// The first `name.exe`, `name.cmd` or `name.bat` in the `PATH` value `path`, folder by
/// folder. Relative entries are skipped. The bare name never counts. npm puts an sh script
/// by that name beside its `.cmd` shim, and Windows cannot start it.
#[cfg(any(windows, test))]
fn find_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    let dirs = std::env::split_paths(path).filter(|dir| dir.is_absolute());
    dirs.flat_map(|dir| ["exe", "cmd", "bat"].map(|ext| dir.join(format!("{name}.{ext}"))))
        .find(|program| std::fs::metadata(program).is_ok_and(|meta| meta.is_file()))
}

/// Runs `claude` with `args`, by [`run`].
fn run_claude(claude: &Path, args: &[String]) -> Result<(), String> {
    let mut command = Command::new(claude);
    command.args(args);
    // AtticDB reads an empty CLAUDE_CONFIG_DIR as unset, so `claude` must too.
    if std::env::var_os(CONFIG_ENV).is_some_and(|dir| dir.is_empty()) {
        command.env_remove(CONFIG_ENV);
    }
    run(&mut command, &shell_line("claude", args), false).map(drop)
}

/// Reads `from` to its end on a thread, so a full pipe or a closed one can't stall or break
/// a chatty child. Sends the last `keep` bytes, and how many bytes came in all.
fn drain(mut from: impl Read + Send + 'static, keep: usize) -> Receiver<(Vec<u8>, usize)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut kept, mut chunk, mut total) = (Vec::new(), [0; 8192], 0);
        loop {
            match from.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    kept.extend_from_slice(&chunk[..n]);
                    kept.drain(..kept.len().saturating_sub(keep));
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = sender.send((kept, total));
    });
    receiver
}

/// Runs `command` with stdin closed, for at most [`CHILD_TIMEOUT`]. `line` names it in
/// errors. It runs in its own process group, so a timeout kills whatever it started too.
/// With `capture`, its stdout is read to its end and returned, up to [`MAX_STDOUT`]. Its
/// stderr is discarded. An error never shows the child's output: `claude` and `codex` can
/// quote their config in an error, and Node's JSON errors quote the text they failed on.
pub(super) fn run(command: &mut Command, line: &str, capture: bool) -> Result<Vec<u8>, String> {
    let stdout = if capture { Stdio::piped() } else { Stdio::null() };
    command.stdin(Stdio::null()).stdout(stdout).stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    crate::platform::no_window(command);
    // Retries take at most 200 ms of the 10 s budget.
    let mut child =
        crate::spawn(command).map_err(|error| format!("{line} could not start: {error}"))?;
    let stdout = child.stdout.take().map(|stdout| drain(stdout, MAX_STDOUT));
    let deadline = Instant::now() + CHILD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                // The group's id is the child's pid, and the child isn't reaped yet, so the
                // id can't have been reused.
                #[cfg(unix)]
                let group = rustix::process::Pid::from_child(&child);
                #[cfg(unix)]
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                #[cfg(windows)]
                crate::platform::kill_tree(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{line} did not finish in 10 s"));
            }
            Err(error) => return Err(format!("{line}: {error}")),
        }
    };
    if !status.success() {
        let code = status.code().map_or_else(|| status.to_string(), |code| format!("exit {code}"));
        return Err(format!("{line} failed ({code})"));
    }
    if !capture {
        return Ok(Vec::new());
    }
    // A process the child left behind may hold the pipe open, so wait for its end only briefly.
    match stdout.and_then(|from| from.recv_timeout(Duration::from_secs(1)).ok()) {
        Some((stdout, total)) if total == stdout.len() => Ok(stdout),
        Some(_) => Err(format!("{line} printed over 1 MiB")),
        None => Err(format!("{line} did not close its output")),
    }
}

/// `word` single-quoted for `sh`, with each `'` written as `'\''`.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The `program` command line for `args`, as typed in a shell.
pub(super) fn shell_line(program: &str, args: &[String]) -> String {
    let plain = |word: &str| {
        !word.is_empty()
            && word
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_./=:@%+,-".contains(&byte))
    };
    let words = args.iter().map(|arg| if plain(arg) { arg.clone() } else { quote(arg) });
    std::iter::once(program.to_owned()).chain(words).collect::<Vec<_>>().join(" ")
}

/// What `harness`'s hook command passes the binary.
pub(super) fn hook_args(harness: Harness) -> String {
    format!(" hook session-start --harness {}", harness.name())
}

/// `harness`'s hook command: the quoted binary, after `ATTICDB_HOME='<home>' ` when `home`
/// is set. Claude's and Codex's command hooks have no env field, so the home goes in the
/// command.
pub(super) fn hook_command(harness: Harness, exe: &str, home: Option<&str>) -> String {
    let prefix = home.map(|home| format!("{HOME_ENV}={} ", quote(home))).unwrap_or_default();
    format!("{prefix}{}{}", quote(exe), hook_args(harness))
}

/// The SessionStart group AtticDB adds. It has no matcher, so it also runs after compaction.
fn hook_group(exe: &str, home: Option<&str>) -> Value {
    let command = hook_command(Harness::ClaudeCode, exe, home);
    json!({"hooks": [{"type": "command", "command": command, "timeout": 5}]})
}

/// Splits a single-quoted word, with `'\''` for each `'`, off the start of `text`. Returns
/// the word and the rest.
fn unquote(text: &str) -> Option<(String, &str)> {
    let mut rest = text.strip_prefix('\'')?;
    let mut word = String::new();
    loop {
        let end = rest.find('\'')?;
        word.push_str(&rest[..end]);
        rest = &rest[end + 1..];
        match rest.strip_prefix(r"\''") {
            Some(after) => {
                word.push('\'');
                rest = after;
            }
            None => return Some((word, rest)),
        }
    }
}

/// Whether `command` is a hook command AtticDB writes for `harness`, for any absolute binary
/// path and any home. The binary's file name doesn't count, as an install may name it
/// `atticdb-0.1.0`.
fn our_command(harness: Harness, command: &str) -> bool {
    command_exe(harness, command).is_some()
}

/// The binary path in `command` when it is a hook command AtticDB writes, by [`our_command`].
fn command_exe(harness: Harness, command: &str) -> Option<String> {
    let (home, rest) = match command.strip_prefix(&format!("{HOME_ENV}=")) {
        Some(rest) => {
            let (home, rest) = unquote(rest)?;
            (Some(home), rest.strip_prefix(' ')?)
        }
        None => (None, command),
    };
    let (exe, rest) = unquote(rest)?;
    // Writing the command again for that binary and home must give the same text.
    let again = hook_command(harness, &exe, home.as_deref());
    let args = hook_args(harness);
    (Path::new(&exe).is_absolute() && rest == args && again == command).then_some(exe)
}

/// The binaries that `harness`'s AtticDB hook commands run in the strict JSON hooks file at
/// `path`, each with `path`. A missing or unreadable file names none.
pub(super) fn hook_binaries(harness: Harness, path: &Path) -> Vec<(PathBuf, String)> {
    let Ok(Some((bytes, _))) = read(path) else { return Vec::new() };
    let root: Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let groups = root["hooks"]["SessionStart"].as_array().cloned().unwrap_or_default();
    let handlers =
        groups.iter().flat_map(|group| group["hooks"].as_array().cloned().unwrap_or_default());
    let exe = |handler: Value| {
        let command = handler["command"].as_str().filter(|_| handler["type"] == "command")?;
        command_exe(harness, command)
    };
    handlers.filter_map(exe).map(|exe| (path.to_owned(), exe)).collect()
}

/// The AtticDB binaries that Claude Code's MCP entry and hook run, for [`super::binaries`].
pub(super) fn binaries() -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    if let Ok(state) = state_file() {
        let exe = current(&state).ok().flatten().and_then(|entry| entry_exe(&entry));
        found.extend(exe.map(|exe| (state, exe)));
    }
    if let Ok(settings) = settings_file() {
        found.extend(hook_binaries(Harness::ClaudeCode, &settings));
    }
    found
}

/// Whether `group` is a SessionStart group whose every handler runs `harness`'s AtticDB hook.
pub(super) fn our_group(harness: Harness, group: &Value) -> bool {
    let handlers = group["hooks"].as_array().map(Vec::as_slice).unwrap_or_default();
    let ours = |handler: &Value| {
        let command = handler["command"].as_str();
        handler["type"] == "command" && command.is_some_and(|command| our_command(harness, command))
    };
    !handlers.is_empty() && handlers.iter().all(ours)
}

/// The allow rules for `notes`.
fn rules(notes: Notes) -> Vec<&'static str> {
    let mut rules = READS.to_vec();
    if notes == Notes::Auto {
        rules.push(APPEND);
    }
    rules
}

/// What an edit to `settings.json` or `hooks.json` did.
pub(super) struct Done {
    /// The new text.
    pub(super) text: String,
    /// What it did, in words.
    said: String,
    /// What it did.
    state: State,
    /// Whether it left something to do by hand, which counts as skipped.
    by_hand: bool,
}

/// An edit to a JSON file's text. `Err` says why it was refused.
pub(super) type Edit = Result<Done, Refused>;

/// Why an edit to a JSON file was refused, and what to do by hand when the part's usual
/// advice doesn't fit.
pub(super) struct Refused {
    /// Why it was refused.
    why: String,
    /// The advice to give instead of the part's.
    by_hand: Option<String>,
}

impl From<jsonedit::Error> for Refused {
    fn from(error: jsonedit::Error) -> Self {
        Self { why: error.to_string(), by_hand: None }
    }
}

/// A `Done` for `text` that did `state` and says `said`.
fn done(text: String, state: State, said: &str) -> Edit {
    Ok(Done { text, said: said.into(), state, by_hand: false })
}

/// `harness`'s AtticDB hook commands in SessionStart groups that also run other hooks. `text`
/// must be strict JSON.
fn mixed(harness: Harness, text: &str) -> Vec<String> {
    let root: Value = serde_json::from_str(text).unwrap_or_default();
    let groups = root["hooks"]["SessionStart"].as_array().cloned().unwrap_or_default();
    let handlers = groups.iter().filter(|group| !our_group(harness, group));
    let handlers =
        handlers.flat_map(|group| group["hooks"].as_array().cloned().unwrap_or_default());
    let ours = |handler: Value| {
        let command = handler["command"].as_str().filter(|command| our_command(harness, command));
        command.filter(|_| handler["type"] == "command").map(str::to_owned)
    };
    handlers.filter_map(ours).collect()
}

/// `text` with `group` in `hooks.SessionStart`, and without AtticDB's other groups. When
/// AtticDB's handler already sits in a group with other hooks, no group is added. An older
/// one there is refused, as it can't be updated without touching the other hooks.
pub(super) fn add_hook(harness: Harness, text: &str, group: &Value) -> Edit {
    let stale = |old: &Value| our_group(harness, old) && old != group;
    let (text, stale) = jsonedit::remove(text, &HOOKS, stale)?;
    let command = group["hooks"][0]["command"].as_str().unwrap_or_default();
    let mixed = mixed(harness, &text);
    if mixed.iter().any(|other| other != command) {
        return Err(Refused {
            why: "an older atticdb hook is inside a group with other hooks".into(),
            by_hand: Some(format!(
                "Update the atticdb handler inside that group by hand, to run: {command}"
            )),
        });
    }
    let root: Value = serde_json::from_str(&text).unwrap_or_default();
    let groups = root["hooks"]["SessionStart"].as_array();
    let state = if stale == 0 { State::Unchanged } else { State::Updated };
    if groups.is_some_and(|groups| groups.contains(group)) {
        return done(text, state, if stale == 0 { "unchanged" } else { "updated" });
    }
    if !mixed.is_empty() {
        return done(text, state, "found inside a group with other hooks, so not added again");
    }
    let text = jsonedit::append(&text, &HOOKS, group)?;
    let state = if stale == 0 { State::Added } else { State::Updated };
    done(text, state, if stale == 0 { "added" } else { "updated" })
}

/// `text` with the allow rules for `notes`. With [`Notes::Ask`], the `memory_append` rule goes.
fn add_rules(text: &str, notes: Notes) -> Edit {
    let ask = notes == Notes::Ask;
    let drop = |rule: &Value| ask && *rule == APPEND;
    let (mut text, dropped) = jsonedit::remove(text, &ALLOW, drop)?;
    let root: Value = serde_json::from_str(&text).unwrap_or_default();
    let have = root["permissions"]["allow"].as_array().cloned().unwrap_or_default();
    let mut added = 0;
    for rule in rules(notes).into_iter().filter(|rule| !have.contains(&json!(rule))) {
        text = jsonedit::append(&text, &ALLOW, &json!(rule))?;
        added += 1;
    }
    let (state, said) = match (dropped, added) {
        (0, 0) => (State::Unchanged, "unchanged"),
        (0, _) => (State::Added, "added"),
        _ => (State::Updated, "updated"),
    };
    done(text, state, said)
}

/// `text` without the SessionStart groups that run `harness`'s AtticDB hook. An AtticDB handler
/// in a group with other hooks stays, and is left to do by hand.
pub(super) fn drop_hooks(harness: Harness, text: &str) -> Edit {
    let ours = |group: &Value| our_group(harness, group);
    let (text, removed) = jsonedit::remove(text, &HOOKS, ours)?;
    let by_hand = !mixed(harness, &text).is_empty();
    let said = match (removed, by_hand) {
        (0, false) => "not found",
        (_, false) => "removed",
        (0, true) => "found inside a group with other hooks; remove it by hand there",
        (_, true) => {
            "removed, but found another inside a group with other hooks; remove that \
                      by hand there"
        }
    };
    let state = if removed == 0 { State::Unchanged } else { State::Removed };
    Ok(Done { text, said: said.into(), state, by_hand })
}

/// `text` without AtticDB's allow rules.
fn drop_rules(text: &str) -> Edit {
    let ours =
        |rule: &Value| rule.as_str().is_some_and(|rule| READS.contains(&rule) || rule == APPEND);
    let (text, removed) = jsonedit::remove(text, &ALLOW, ours)?;
    match removed {
        0 => done(text, State::Unchanged, "not found"),
        _ => done(text, State::Removed, "removed"),
    }
}

/// A part that lives in a JSON file, such as `settings.json`.
pub(super) struct Part<'a> {
    /// Its name in the printed line.
    pub(super) name: &'static str,
    /// What to do by hand when it is skipped.
    pub(super) by_hand: String,
    /// The edit that applies it.
    pub(super) edit: &'a dyn Fn(&str) -> Edit,
}

/// Claude's `settings.json`, in its config dir.
pub(crate) fn settings_file() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("settings.json"))
}

/// Applies `parts` to the JSON file at `path` in turn, then writes the file once, with a
/// backup, if it changed. A part whose edit is refused is skipped, and the others still
/// apply. A missing file starts as `empty`. Returns 1 line per part. In Plan mode, it
/// writes nothing.
pub(super) fn edit_file(
    path: Result<PathBuf, String>,
    empty: &str,
    parts: &[Part<'_>],
    mode: Mode,
) -> Vec<Line> {
    let skip_all = |why: &str| {
        let skip = |part: &Part<'_>| {
            Line::new(State::Skipped, format!("{}: skipped, {why}. {}", part.name, part.by_hand))
        };
        parts.iter().map(skip).collect()
    };
    let path = match path {
        Ok(path) => path,
        Err(why) => return skip_all(&why),
    };
    let shown = path.display();
    let original = match read(&path) {
        Ok(Some((bytes, mode))) => match String::from_utf8(bytes) {
            Ok(text) => Some((text, mode)),
            Err(_) => return skip_all(&format!("{shown} is not UTF-8")),
        },
        Ok(None) => None,
        Err(why) => return skip_all(&format!("{shown} {why}")),
    };
    let start = original.as_ref().map_or(empty, |(text, _)| text.as_str());
    let mut text = start.to_owned();
    // Per part: what it did, and whether it changed the text. Or why it was skipped or
    // failed, and what to do by hand when the part's advice doesn't fit.
    let mut done = Vec::new();
    for part in parts {
        done.push(match (part.edit)(&text) {
            Ok(edit) => {
                let changed = edit.text != text;
                text = edit.text;
                Ok((edit.said, edit.state, edit.by_hand, changed))
            }
            Err(refused) => {
                Err((State::Skipped, format!("{shown}: {}", refused.why), refused.by_hand))
            }
        });
    }
    let mut backup = None;
    if text != start && mode == Mode::Apply {
        let old = original.map(|(text, mode)| (text.into_bytes(), mode));
        match write(&path, old.as_ref(), text.as_bytes()) {
            Ok(saved) => backup = saved,
            Err(why) => {
                for done in done.iter_mut().filter(|done| matches!(done, Ok((.., true)))) {
                    *done = Err((State::Failed, why.clone(), None));
                }
            }
        }
    }
    type Outcome = Result<(String, State, bool, bool), (State, String, Option<String>)>;
    let line = |(part, done): (&Part<'_>, Outcome)| match done {
        Ok((_, state, false, _)) if mode == Mode::Plan => planned(&path, part.name, state),
        Ok((said, state, by_hand, changed)) => {
            let line = match (&backup, changed) {
                (Some(saved), true) => {
                    format!("{}: {said} ({shown}, backup: {})", part.name, saved.display())
                }
                _ => format!("{}: {said} ({shown})", part.name),
            };
            Line::new(if by_hand { State::Skipped } else { state }, line)
        }
        Err((state, why, by_hand)) => {
            let by_hand = by_hand.as_deref().unwrap_or(&part.by_hand);
            Line::new(state, format!("{}: skipped, {why}. {by_hand}", part.name))
        }
    };
    parts.iter().zip(done).map(line).collect()
}

#[cfg(test)]
mod find_tests {
    #![allow(clippy::unwrap_used)]

    /// npm's layout: an sh script `claude` and a `claude.cmd` shim.
    #[test]
    fn find_in_skips_the_bare_name() {
        let dir = std::env::temp_dir().join(format!("atticdb-find-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = std::env::join_paths([&dir]).unwrap();
        std::fs::write(dir.join("claude"), "#!/bin/sh\n").unwrap();
        assert_eq!(super::find_in("claude", &path), None);
        std::fs::write(dir.join("claude.bat"), "").unwrap();
        assert_eq!(super::find_in("claude", &path), Some(dir.join("claude.bat")));
        std::fs::write(dir.join("claude.cmd"), "").unwrap();
        assert_eq!(super::find_in("claude", &path), Some(dir.join("claude.cmd")));
        std::fs::write(dir.join("claude.exe"), "").unwrap();
        assert_eq!(super::find_in("claude", &path), Some(dir.join("claude.exe")));
        let relative = std::env::join_paths(["relative"]).unwrap();
        assert_eq!(super::find_in("claude", &relative), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
