//! Claude Code's 3 parts:
//!
//! - The MCP entry. Claude keeps it in `.claude.json` and rewrites that file itself, so zerodb
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
    absolute_env, binary, entry_home, home_words, moved_home, read, refuse_flags, write, Harness,
    COPILOT_SHARED,
};
use crate::home::{user_home, HOME_ENV};
use crate::jsonedit;
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Whether Claude Code asks before zerodb saves a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notes {
    /// Claude Code asks before each `memory_append`.
    Ask,
    /// `memory_append` is allowed, so notes are saved without asking.
    Auto,
}

/// The read tools Claude Code may call without asking.
const READS: [&str; 4] = [
    "mcp__zerodb__memory_search",
    "mcp__zerodb__memory_recent",
    "mcp__zerodb__fact_get",
    "mcp__zerodb__fact_list",
];
/// The rule that saves notes without asking.
const APPEND: &str = "mcp__zerodb__memory_append";
/// Where the hook groups go in `settings.json`.
const HOOKS: [&str; 2] = ["hooks", "SessionStart"];
/// Where the allow rules go.
const ALLOW: [&str; 2] = ["permissions", "allow"];
/// What the hook command passes the binary.
const HOOK_ARGS: &str = " hook session-start --harness claude-code";
/// The `claude` args that remove zerodb's entry.
const REMOVE: [&str; 5] = ["mcp", "remove", "-s", "user", "zerodb"];
/// The most of `.claude.json` read. It holds all of Claude's state, so it can be large.
const MAX_STATE: u64 = 64 << 20;
/// How long a `claude` command may run.
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(10);

/// What a part did, as the line to print. `Err` when it was skipped or failed.
type Line = Result<String, String>;

/// `$CLAUDE_CONFIG_DIR` when set and absolute, else `~/.claude`.
fn config_dir() -> Result<PathBuf, String> {
    absolute_env("CLAUDE_CONFIG_DIR").map_or_else(|| Ok(user_home()?.join(".claude")), Ok)
}

/// Claude's state file, which holds the user MCP servers: `.claude.json` in
/// `$CLAUDE_CONFIG_DIR` when set and absolute, else in `~`.
pub(super) fn state_file() -> Result<PathBuf, String> {
    let dir = absolute_env("CLAUDE_CONFIG_DIR").map_or_else(user_home, Ok)?;
    Ok(dir.join(".claude.json"))
}

/// Adds the MCP entry, the hook when `hook`, and the allow rules for `notes`. An entry that
/// differs is refused before anything changes, unless `force`. Returns the exit code.
pub(super) fn connect(force: bool, hook: bool, notes: Notes) -> u8 {
    let prepared = (|| {
        let home = moved_home(Harness::ClaudeCode)?;
        let exe = binary()?;
        let state = state_file()?;
        let current = current(&state);
        if let (false, Ok(Some(entry))) = (force, &current) {
            if !same(entry, &exe, home.as_deref()) && !updatable(entry) {
                let data = crate::home::dir()?.join("data");
                refuse_flags(Harness::ClaudeCode, &state, entry, &data)?;
                return Err(differs(&state, entry, &exe, home.as_deref()));
            }
        }
        let warnings = split(home.as_deref());
        crate::home::ensure()?;
        Ok((home, exe, state, current, warnings))
    })();
    let (home, exe, state, current, warnings) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("zerodb: {error}");
            return 1;
        }
    };
    let home = home.as_deref();
    let entry = match current {
        Ok(current) => set_entry(&state, current.as_ref(), &exe, home),
        Err(why) => Err(format!(
            "MCP entry: skipped, {}: {why}. Run by hand: {}",
            state.display(),
            shell_line(&add_args(&exe, home))
        )),
    };
    let group = hook_group(&exe, home);
    let add_group = |text: &str| add_hook(text, &group);
    let add_allow = |text: &str| add_rules(text, notes);
    let mut parts = Vec::new();
    if hook {
        let by_hand = format!("Add this group to hooks.SessionStart by hand: {group}");
        parts.push(Part { name: "SessionStart hook", by_hand, edit: &add_group });
    }
    let by_hand = format!("Add these to permissions.allow by hand: {}", json!(rules(notes)));
    parts.push(Part { name: "allow rules", by_hand, edit: &add_allow });
    let mut lines = vec![entry];
    lines.extend(edit_settings(&parts));
    report(lines, &warnings, Some("Start a new Claude Code session to load zerodb."))
}

/// Removes the MCP entry when it is what zerodb writes, every hook group that runs zerodb's
/// hook, and zerodb's allow rules. Returns the exit code.
pub(super) fn disconnect() -> u8 {
    let state = match state_file() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("zerodb: {error}");
            return 1;
        }
    };
    let mut every = READS.to_vec();
    every.push(APPEND);
    let parts = [
        Part {
            name: "SessionStart hook",
            by_hand: format!(
                "Remove the groups that run zerodb{HOOK_ARGS} from hooks.SessionStart by hand"
            ),
            edit: &drop_hooks,
        },
        Part {
            name: "allow rules",
            by_hand: format!("Remove these from permissions.allow by hand: {}", json!(every)),
            edit: &drop_rules,
        },
    ];
    let mut lines = vec![drop_entry(&state)];
    lines.extend(edit_settings(&parts));
    report(lines, &[], None)
}

/// Prints `lines` in order, then `warnings` on stderr, then `next`. Returns 1 when a part
/// was skipped or failed, else 0.
fn report(lines: Vec<Line>, warnings: &[String], next: Option<&str>) -> u8 {
    let mut code = 0;
    for line in lines {
        match line {
            Ok(line) => println!("{line}"),
            Err(line) => {
                println!("{line}");
                code = 1;
            }
        }
    }
    for warning in warnings {
        eprintln!("zerodb: {warning}");
    }
    if let Some(next) = next {
        println!("{next}");
    }
    code
}

/// Notes on the other harnesses that pass a home other than `home`. They share no file with
/// Claude Code, so connecting goes ahead, but memory splits between the homes.
fn split(home: Option<&str>) -> Vec<String> {
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
            "{} uses {theirs}, but claude-code now uses {ours}. Memory splits: each harness \
             reads and saves only its own home. Connect both under the same {HOME_ENV} to share it",
            other.name()
        ))
    };
    COPILOT_SHARED.into_iter().filter_map(other).collect()
}

/// The args zerodb's entry passes.
fn mcp_args() -> Value {
    json!(["mcp", "--agent", Harness::ClaudeCode.agent()])
}

/// The `ZERODB_HOME` an entry passes. An empty one counts as none.
fn home_of(entry: &Value) -> Option<&str> {
    entry["env"][HOME_ENV].as_str().filter(|home| !home.is_empty())
}

/// Whether `entry` runs `exe` with zerodb's args on `home`.
fn same(entry: &Value, exe: &str, home: Option<&str>) -> bool {
    entry["command"] == exe
        && entry["args"] == mcp_args()
        && home_of(entry).map(Path::new) == home.map(Path::new)
}

/// Whether `entry` runs any absolute path with exactly zerodb's args, so it has no
/// connection flags. `connect` updates it without `--force`, as for VS Code and Copilot.
fn updatable(entry: &Value) -> bool {
    let command = entry["command"].as_str().map(Path::new);
    command.is_some_and(Path::is_absolute) && entry["args"] == mcp_args()
}

/// Whether `entry` is what zerodb writes, for any absolute `zerodb` binary and any home.
fn written(entry: &Value) -> bool {
    let Some(entry) = entry.as_object() else { return false };
    let command = entry.get("command").and_then(Value::as_str).map(Path::new);
    let zerodb = command.is_some_and(|command| {
        command.is_absolute() && command.file_name() == Some(OsStr::new("zerodb"))
    });
    let known = entry.keys().all(|key| ["type", "command", "args", "env"].contains(&key.as_str()));
    let stdio = entry.get("type").is_none_or(|kind| kind == "stdio");
    let env = entry
        .get("env")
        .is_none_or(|env| env.as_object().is_some_and(|env| env.keys().all(|key| key == HOME_ENV)));
    zerodb && known && stdio && env && entry.get("args") == Some(&mcp_args())
}

/// Why `entry`, the entry in `state`, is not replaced without `--force`. It shows the
/// command, args and home, never the rest of its env.
fn differs(state: &Path, entry: &Value, exe: &str, home: Option<&str>) -> String {
    let shown = state.display();
    let (command, args) = (&entry["command"], &entry["args"]);
    format!(
        "{shown}: the zerodb entry for claude-code runs {command} {args} on {}, not a zerodb \
         mcp command such as {} {} on {}. Nothing changed. To replace it: zerodb connect \
         claude-code --force",
        home_words(home_of(entry)),
        json!(exe),
        mcp_args(),
        home_words(home)
    )
}

/// zerodb's entry in the state file at `path`, `None` when there is none. A symlink or
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
    Ok(root.get("mcpServers").and_then(|servers| servers.get("zerodb")).cloned())
}

/// The `claude` args that add zerodb's entry for `exe`, passing `home` when set.
fn add_args(exe: &str, home: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["mcp", "add", "-s", "user"].map(String::from).to_vec();
    if let Some(home) = home {
        args.extend(["-e".into(), format!("{HOME_ENV}={home}")]);
    }
    let agent = Harness::ClaudeCode.agent();
    args.extend(["zerodb", "--", exe, "mcp", "--agent", agent].map(String::from));
    args
}

/// Makes zerodb's entry run `exe` on `home`, through `claude`. `current` is the entry read
/// before. One that differs, such as one from an old binary path, is removed first.
fn set_entry(state: &Path, current: Option<&Value>, exe: &str, home: Option<&str>) -> Line {
    let shown = state.display();
    if current.is_some_and(|current| same(current, exe, home)) {
        return Ok(format!("MCP entry: unchanged ({shown})"));
    }
    let mut steps = Vec::new();
    if current.is_some() {
        steps.push(REMOVE.map(String::from).to_vec());
    }
    steps.push(add_args(exe, home));
    let by_hand = steps.iter().map(|step| shell_line(step)).collect::<Vec<_>>().join(" && ");
    let claude = find_claude()
        .ok_or_else(|| format!("MCP entry: skipped, claude is not on PATH. Run: {by_hand}"))?;
    let failed = |why: String| format!("MCP entry: failed, {why}. Run by hand: {by_hand}");
    for step in &steps {
        run_claude(&claude, step).map_err(failed)?;
    }
    let verb = if current.is_some() { "updated" } else { "added" };
    match self::current(state) {
        Ok(Some(entry)) if same(&entry, exe, home) => Ok(format!("MCP entry: {verb} ({shown})")),
        Ok(_) => Err(failed(format!("{shown} does not show the entry after claude ran"))),
        Err(why) => Err(failed(format!("{shown} {why}"))),
    }
}

/// Removes zerodb's entry through `claude` when it is what zerodb writes.
fn drop_entry(state: &Path) -> Line {
    let shown = state.display();
    let by_hand = shell_line(&REMOVE.map(String::from));
    let entry = current(state)
        .map_err(|why| format!("MCP entry: skipped, {shown} {why}. Run by hand: {by_hand}"))?;
    let Some(entry) = entry else { return Ok(format!("MCP entry: not found ({shown})")) };
    if !written(&entry) {
        return Err(format!(
            "MCP entry: skipped, it differs from the entry zerodb writes ({shown}). To remove it \
             anyway, run: {by_hand}"
        ));
    }
    let claude = find_claude()
        .ok_or_else(|| format!("MCP entry: skipped, claude is not on PATH. Run: {by_hand}"))?;
    let failed = |why: String| format!("MCP entry: failed, {why}. Run by hand: {by_hand}");
    run_claude(&claude, &REMOVE.map(String::from)).map_err(failed)?;
    match current(state) {
        Ok(None) => Ok(format!("MCP entry: removed ({shown})")),
        Ok(Some(_)) => Err(failed(format!("{shown} still shows the entry after claude ran"))),
        Err(why) => Err(failed(format!("{shown} {why}"))),
    }
}

/// The first `claude` on `PATH` that is an executable regular file. Relative `PATH` entries
/// are skipped.
fn find_claude() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs = std::env::split_paths(&path).filter(|dir| dir.is_absolute());
    dirs.map(|dir| dir.join("claude")).find(|claude| {
        std::fs::metadata(claude)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

/// Runs `claude` with `args` and stdin closed, for at most [`CLAUDE_TIMEOUT`]. An error
/// ends with the last line `claude` printed on stderr.
fn run_claude(claude: &Path, args: &[String]) -> Result<(), String> {
    let line = shell_line(args);
    let mut child = Command::new(claude)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{line} could not start: {error}"))?;
    // Read on a thread, so a full pipe can't stall `claude` past the timeout.
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.take(64 << 10).read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        });
    }
    let deadline = Instant::now() + CLAUDE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{line} did not finish in 10 s"));
            }
            Err(error) => return Err(format!("{line}: {error}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    // A process `claude` left behind may hold stderr open, so wait for it only briefly.
    let stderr = receiver.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr);
    let last = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default();
    Err(format!("{line} exited with {status}: {}", last.trim()))
}

/// `word` single-quoted for `sh`, with each `'` written as `'\''`.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The `claude` command line for `args`, as typed in a shell.
fn shell_line(args: &[String]) -> String {
    let plain = |word: &str| {
        !word.is_empty()
            && word
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_./=:@%+,-".contains(&byte))
    };
    let words = args.iter().map(|arg| if plain(arg) { arg.clone() } else { quote(arg) });
    std::iter::once("claude".to_owned()).chain(words).collect::<Vec<_>>().join(" ")
}

/// The hook's command: the quoted binary, after `ZERODB_HOME='<home>' ` when `home` is set.
/// Claude's command hooks have no env field, so the home goes in the command.
fn hook_command(exe: &str, home: Option<&str>) -> String {
    let prefix = home.map(|home| format!("{HOME_ENV}={} ", quote(home))).unwrap_or_default();
    format!("{prefix}{}{HOOK_ARGS}", quote(exe))
}

/// The SessionStart group zerodb adds. It has no matcher, so it also runs after compaction.
fn hook_group(exe: &str, home: Option<&str>) -> Value {
    json!({"hooks": [{"type": "command", "command": hook_command(exe, home), "timeout": 5}]})
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

/// Whether `command` is a hook command zerodb writes, for any absolute `zerodb` binary and
/// any home.
fn our_command(command: &str) -> bool {
    let parse = || {
        let (home, rest) = match command.strip_prefix(&format!("{HOME_ENV}=")) {
            Some(rest) => {
                let (home, rest) = unquote(rest)?;
                (Some(home), rest.strip_prefix(' ')?)
            }
            None => (None, command),
        };
        let (exe, rest) = unquote(rest)?;
        let path = Path::new(&exe);
        let zerodb = path.is_absolute() && path.file_name() == Some(OsStr::new("zerodb"));
        // Writing the command again for that binary and home must give the same text.
        let again = hook_command(&exe, home.as_deref());
        (zerodb && rest == HOOK_ARGS && again == command).then_some(())
    };
    parse().is_some()
}

/// Whether `group` is a SessionStart group whose every handler runs zerodb's hook.
fn our_group(group: &Value) -> bool {
    let handlers = group["hooks"].as_array().map(Vec::as_slice).unwrap_or_default();
    let ours = |handler: &Value| {
        handler["type"] == "command" && handler["command"].as_str().is_some_and(our_command)
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

/// An edit to `settings.json`'s text: the new text, and what it did.
type Edit = Result<(String, &'static str), jsonedit::Error>;

/// `text` with `group` in `hooks.SessionStart`, and without zerodb's other groups.
fn add_hook(text: &str, group: &Value) -> Edit {
    let (text, stale) = jsonedit::remove(text, &HOOKS, |old| our_group(old) && old != group)?;
    let root: Value = serde_json::from_str(&text).unwrap_or_default();
    let groups = root["hooks"]["SessionStart"].as_array();
    if groups.is_some_and(|groups| groups.contains(group)) {
        return Ok((text, if stale == 0 { "unchanged" } else { "updated" }));
    }
    let text = jsonedit::append(&text, &HOOKS, group)?;
    Ok((text, if stale == 0 { "added" } else { "updated" }))
}

/// `text` with the allow rules for `notes`. With [`Notes::Ask`], the `memory_append` rule goes.
fn add_rules(text: &str, notes: Notes) -> Edit {
    let ask = notes == Notes::Ask;
    let (mut text, dropped) = jsonedit::remove(text, &ALLOW, |rule| ask && *rule == APPEND)?;
    let root: Value = serde_json::from_str(&text).unwrap_or_default();
    let have = root["permissions"]["allow"].as_array().cloned().unwrap_or_default();
    let mut added = 0;
    for rule in rules(notes).into_iter().filter(|rule| !have.contains(&json!(rule))) {
        text = jsonedit::append(&text, &ALLOW, &json!(rule))?;
        added += 1;
    }
    Ok((
        text,
        if dropped > 0 {
            "updated"
        } else if added > 0 {
            "added"
        } else {
            "unchanged"
        },
    ))
}

/// `text` without the SessionStart groups that run zerodb's hook.
fn drop_hooks(text: &str) -> Edit {
    let (text, removed) = jsonedit::remove(text, &HOOKS, our_group)?;
    Ok((text, if removed == 0 { "not found" } else { "removed" }))
}

/// `text` without zerodb's allow rules.
fn drop_rules(text: &str) -> Edit {
    let ours =
        |rule: &Value| rule.as_str().is_some_and(|rule| READS.contains(&rule) || rule == APPEND);
    let (text, removed) = jsonedit::remove(text, &ALLOW, ours)?;
    Ok((text, if removed == 0 { "not found" } else { "removed" }))
}

/// A part that lives in `settings.json`.
struct Part<'a> {
    /// Its name in the printed line.
    name: &'static str,
    /// What to do by hand when it is skipped.
    by_hand: String,
    /// The edit that applies it.
    edit: &'a dyn Fn(&str) -> Edit,
}

/// Applies `parts` to `settings.json` in turn, then writes the file once, with a backup, if
/// it changed. A part whose edit is refused is skipped, and the others still apply. A
/// missing file starts as `{}`. Returns 1 line per part.
fn edit_settings(parts: &[Part<'_>]) -> Vec<Line> {
    let skip_all = |why: &str| {
        let skip =
            |part: &Part<'_>| Err(format!("{}: skipped, {why}. {}", part.name, part.by_hand));
        parts.iter().map(skip).collect()
    };
    let path = match config_dir() {
        Ok(dir) => dir.join("settings.json"),
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
    let start = original.as_ref().map_or("{\n}\n", |(text, _)| text.as_str());
    let mut text = start.to_owned();
    let mut done = Vec::new();
    for part in parts {
        done.push(match (part.edit)(&text) {
            Ok((edited, verb)) => {
                text = edited;
                Ok(verb)
            }
            Err(error) => Err(format!("{shown}: {error}")),
        });
    }
    let mut backup = None;
    if text != start {
        let old = original.map(|(text, mode)| (text.into_bytes(), mode));
        match write(&path, old.as_ref(), text.as_bytes()) {
            Ok(saved) => backup = saved,
            Err(why) => {
                let changed = |done: &Result<&str, String>| {
                    done.as_ref().is_ok_and(|verb| !["unchanged", "not found"].contains(verb))
                };
                for done in done.iter_mut().filter(|done| changed(done)) {
                    *done = Err(why.clone());
                }
            }
        }
    }
    let line = |(part, done): (&Part<'_>, Result<&str, String>)| match done {
        Ok(verb @ ("unchanged" | "not found")) => Ok(format!("{}: {verb} ({shown})", part.name)),
        Ok(verb) => Ok(match &backup {
            Some(saved) => format!("{}: {verb} ({shown}, backup: {})", part.name, saved.display()),
            None => format!("{}: {verb} ({shown})", part.name),
        }),
        Err(why) => Err(format!("{}: skipped, {why}. {}", part.name, part.by_hand)),
    };
    parts.iter().zip(done).map(line).collect()
}
