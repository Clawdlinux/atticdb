//! Gemini CLI's 3 parts, in its config dir: `$GEMINI_CLI_HOME/.gemini` when set, else
//! `~/.gemini`.
//!
//! - The MCP entry, under `mcpServers` in `settings.json`, as [`super::plain`] writes it. Its
//!   `trust` skips Gemini CLI's confirmation before each atticdb tool call, unless notes ask.
//! - The SessionStart hook, a group in `hooks.SessionStart` of the same file, found by its
//!   content, as for Claude Code. Its `timeout` is in milliseconds.
//! - The guide: the MCP server's instructions as a block in `GEMINI.md`. See
//!   [`super::guide`].
//!
//! People edit `settings.json` by hand, so [`crate::jsonedit`] edits it and changes only the
//! bytes it must. A file that isn't strict JSON is never rewritten. Its lines say what to do
//! by hand.

use super::claude::{add_hook, drop_hooks, edit_file, hook_binaries, hook_command, Event, Part};
use super::guide::{self, GEMINI as GUIDE};
use super::plain::{self, Prepared, EMPTY, PART};
use super::{absolute_env, entry_binary, Harness, Mode, Notes, Report};
use crate::home::user_home;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The variable that moves Gemini CLI's home, the dir that holds its `.gemini`.
const HOME_ENV: &str = "GEMINI_CLI_HOME";
/// The hook's name, which Gemini CLI shows in its logs and commands.
const NAME: &str = "atticdb";
/// The hook's timeout in milliseconds, as Gemini CLI reads it. The hook stops itself at 4 s.
const TIMEOUT_MS: u64 = 5_000;
/// What to do after connecting.
const NEXT: &str = "Start a new Gemini CLI session to load atticdb.";

/// Refuses a relative `GEMINI_CLI_HOME`. Gemini CLI reads it from its own working dir, which
/// AtticDB can't know. An empty one counts as unset, as it does for Gemini CLI.
fn check_env() -> Result<(), String> {
    match std::env::var_os(HOME_ENV) {
        Some(dir) if !dir.is_empty() && !Path::new(&dir).is_absolute() => Err(format!(
            "{HOME_ENV} {} must be an absolute path. Nothing changed",
            Path::new(&dir).display()
        )),
        _ => Ok(()),
    }
}

/// `.gemini` in `$GEMINI_CLI_HOME` when set and absolute, else in `~`.
pub(super) fn config_dir() -> Result<PathBuf, String> {
    let home = absolute_env(HOME_ENV).map_or_else(user_home, Ok)?;
    Ok(home.join(".gemini"))
}

/// Gemini CLI's user `settings.json`, which holds the MCP servers and the hooks.
pub(super) fn settings_file() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("settings.json"))
}

/// The entry that runs `exe` on `home`. With [`Notes::Ask`] it has no `trust`, so Gemini CLI
/// asks before each atticdb tool call, notes included.
fn entry(exe: &str, home: Option<&str>, notes: Notes) -> Value {
    let mut entry = Harness::Gemini.entry(exe, home);
    if let (Notes::Ask, Some(entry)) = (notes, entry.as_object_mut()) {
        entry.remove("trust");
    }
    entry
}

/// The group AtticDB adds. It has no matcher, so it runs on startup, resume and `/clear`.
fn hook_group(exe: &str, home: Option<&str>) -> Value {
    let command = hook_command(Harness::Gemini, Event::SessionStart, exe, home);
    json!({"hooks": [{"type": "command", "name": NAME, "command": command,
        "timeout": TIMEOUT_MS}]})
}

/// Adds the MCP entry for `notes`, the hook when `hook`, and the guide. An entry with
/// connection flags is refused before anything changes, unless `force`. It stops with exit
/// code 2 for a relative `GEMINI_CLI_HOME`. In Plan mode, it writes nothing.
pub(super) fn connect(force: bool, hook: bool, notes: Notes, mode: Mode) -> Report {
    if let Err(error) = check_env() {
        return Report::stopped(2, error);
    }
    let prepared =
        config_dir().and_then(|dir| Ok((dir, plain::prepare(Harness::Gemini, force, mode)?)));
    let (dir, Prepared { path, home, exe, mut warnings }) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Report::stopped(1, error),
    };
    let home = home.as_deref();
    let entry = entry(&exe, home, notes);
    let group = hook_group(&exe, home);
    let set = |text: &str| plain::set_entry(Harness::Gemini, text, &entry);
    let add = |text: &str| add_hook(Harness::Gemini, Event::SessionStart, text, &group);
    let by_hand = plain::add_by_hand(Harness::Gemini, &entry);
    let mut parts = vec![Part { name: PART, by_hand, edit: &set, adds: None }];
    if hook {
        let (name, by_hand) = (Event::SessionStart.part(), Event::SessionStart.add_by_hand(&group));
        parts.push(Part { name, by_hand, edit: &add, adds: None });
    }
    let mut lines = edit_file(Ok(path), EMPTY, &parts, mode);
    // The hook is the second part, when it was asked for.
    let hook_state = lines.get(1).filter(|_| hook).map(|line| line.state);
    let (guide, too_big) = guide::add(&GUIDE, &dir, mode);
    lines.push(guide);
    warnings.extend(too_big);
    let next = match mode {
        Mode::Apply => vec![NEXT.into()],
        Mode::Plan => Vec::new(),
    };
    Report { lines, warnings, next, hook: hook_state, ..Report::default() }
}

/// Removes the MCP entry, whatever it runs, every SessionStart group that runs AtticDB's
/// hook, and the guide's block. Returns the exit code: 2 for a relative `GEMINI_CLI_HOME`.
pub(super) fn disconnect() -> u8 {
    if let Err(error) = check_env() {
        eprintln!("atticdb: {error}");
        return 2;
    }
    let dir = match config_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("atticdb: {error}");
            return 1;
        }
    };
    let event = Event::SessionStart;
    let drop_entry = |text: &str| plain::drop_entry(Harness::Gemini, text);
    let drop_group = |text: &str| drop_hooks(Harness::Gemini, event, text);
    let parts = [
        Part {
            name: PART,
            by_hand: plain::drop_by_hand(Harness::Gemini),
            edit: &drop_entry,
            adds: None,
        },
        Part {
            name: event.part(),
            by_hand: event.drop_by_hand(Harness::Gemini),
            edit: &drop_group,
            adds: None,
        },
    ];
    let mut lines = edit_file(Ok(dir.join("settings.json")), EMPTY, &parts, Mode::Apply);
    lines.extend(guide::remove(&GUIDE, &dir));
    Report { lines, ..Report::default() }.print()
}

/// The AtticDB binaries that Gemini CLI's MCP entry and hook run, for [`super::binaries`]. A
/// binary shows once per file, though both parts may run it.
pub(super) fn binaries() -> Vec<(PathBuf, String)> {
    let mut found: Vec<_> = entry_binary(Harness::Gemini).into_iter().collect();
    if let Ok(settings) = settings_file() {
        for binary in hook_binaries(Harness::Gemini, Event::SessionStart, &settings) {
            if !found.contains(&binary) {
                found.push(binary);
            }
        }
    }
    found
}
