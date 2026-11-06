//! The apps that get only AtticDB's MCP entry: Antigravity and Claude Desktop. Each keeps its
//! servers under `mcpServers` in 1 JSON file, and reads it only at start.
//!
//! People edit these files by hand, so [`crate::jsonedit`] edits them and changes only the
//! bytes it must. A file that isn't strict JSON is never rewritten. Its line says what to do
//! by hand. The entry follows Copilot CLI's rules: an older one is updated in place, and one
//! with connection flags is replaced only with `--force`. Disconnect removes only the entry,
//! so an emptied `mcpServers` stays as `{}`.

use super::claude::{done, edit_file, split, Edit, Part};
use super::{absolute_env, binary, check_flags, moved_home, Harness, Mode, Report, State};
use crate::home::user_home;
use crate::jsonedit;
use serde_json::Value;
use std::path::PathBuf;

/// What a missing config starts as.
const EMPTY: &str = "{\n}\n";

/// Why Claude Desktop can't be connected on this system.
const NO_DESKTOP: &str = "Claude Desktop is only available on macOS and Windows";

/// `~/.gemini`, which holds Gemini CLI's config and Antigravity's dir.
pub(super) fn gemini_dir() -> Result<PathBuf, String> {
    Ok(user_home()?.join(".gemini"))
}

/// Claude Desktop's config dir: `~/Library/Application Support/Claude` on macOS, and
/// `%APPDATA%\Claude` on Windows. Other systems have no Claude Desktop app.
pub(super) fn desktop_dir() -> Result<PathBuf, String> {
    if cfg!(target_os = "macos") {
        Ok(user_home()?.join("Library/Application Support/Claude"))
    } else if cfg!(windows) {
        let appdata = absolute_env("APPDATA").ok_or("APPDATA must be set to an absolute path")?;
        Ok(appdata.join("Claude"))
    } else {
        Err(NO_DESKTOP.into())
    }
}

/// What connecting or disconnecting a harness that has no parts yet says.
pub(super) fn not_yet(harness: Harness) -> String {
    format!("{} is not supported yet. Nothing changed", harness.name())
}

/// Adds `harness`'s MCP entry to its config, after creating the home. Under `ATTICDB_HOME`,
/// the entry passes it on. An entry with connection flags is refused before anything
/// changes, unless `force`. In Apply mode, an entry added or updated ends the report with
/// `Restart <App> to load it.` In Plan mode, it writes nothing.
pub(super) fn connect(harness: Harness, force: bool, mode: Mode) -> Report {
    let prepared = (|| {
        let path = harness.file().map_err(|why| format!("{why}. Nothing changed"))?;
        let home = moved_home(harness)?;
        if !force {
            check_flags(harness, &crate::home::dir()?.join("data"))?;
        }
        // Plan mode must not create the home.
        match mode {
            Mode::Apply => drop(crate::home::ensure()?),
            Mode::Plan => crate::home::check_existing()?,
        }
        let entry = harness.entry(&binary()?, home.as_deref());
        Ok::<_, String>((path, entry, split(harness, home.as_deref())))
    })();
    let (path, entry, warnings) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Report::stopped(1, error),
    };
    let set = |text: &str| set_entry(harness, text, &entry);
    let by_hand = format!("Add \"atticdb\": {entry} to \"{}\" by hand", harness.section());
    let part = Part { name: "MCP entry", by_hand, edit: &set, adds: None };
    let lines = edit_file(Ok(path), EMPTY, &[part], mode);
    let changed = |state: State| matches!(state, State::Added | State::Updated);
    let restart = mode == Mode::Apply && lines.iter().any(|line| changed(line.state));
    let next =
        if restart { vec![format!("Restart {} to load it.", harness.title())] } else { Vec::new() };
    Report { lines, warnings, next, ..Report::default() }
}

/// Removes `harness`'s MCP entry from its config, whatever it runs, as for Copilot CLI.
/// Returns the exit code.
pub(super) fn disconnect(harness: Harness) -> u8 {
    let path = match harness.file() {
        Ok(path) => path,
        Err(why) => {
            eprintln!("atticdb: {why}");
            return 1;
        }
    };
    let drop = |text: &str| drop_entry(harness, text);
    let by_hand = format!("Remove \"atticdb\" from \"{}\" by hand", harness.section());
    let part = Part { name: "MCP entry", by_hand, edit: &drop, adds: None };
    let lines = edit_file(Ok(path), EMPTY, &[part], Mode::Apply);
    Report { lines, ..Report::default() }.print()
}

/// `text` with `entry` as `harness`'s AtticDB entry. One that differs is replaced where it
/// stands, so the other servers keep their places and bytes. A new one goes last in the
/// section, which is created when missing.
fn set_entry(harness: Harness, text: &str, entry: &Value) -> Edit {
    let section = harness.section();
    let (replaced, had) = jsonedit::replace_key(text, &[section], "atticdb", entry)?;
    if !had {
        let text = jsonedit::add_key(text, &[section], "atticdb", entry)?;
        return done(text, State::Added, "added");
    }
    // `text` is strict JSON here, as `replace_key` parsed it.
    let root: Value = serde_json::from_str(text).unwrap_or_default();
    if root[section]["atticdb"] == *entry {
        return done(text.to_owned(), State::Unchanged, "unchanged");
    }
    done(replaced, State::Updated, "updated")
}

/// `text` without `harness`'s AtticDB entry. Only the `atticdb` key goes, never the section.
/// So a user's own empty section comes back byte for byte. A config that had no section
/// before connect keeps an empty one.
fn drop_entry(harness: Harness, text: &str) -> Edit {
    let (text, had) = jsonedit::remove_key(text, &[harness.section()], "atticdb")?;
    if had {
        done(text, State::Removed, "removed")
    } else {
        done(text, State::Unchanged, "not found")
    }
}
