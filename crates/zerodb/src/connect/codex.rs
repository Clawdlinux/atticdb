//! Codex's 2 parts, in `$CODEX_HOME`, else `~/.codex`:
//!
//! - The MCP entry with its tool approvals: a block between 2 marker lines at the end of
//!   `config.toml`. `codex mcp add` can't set approvals, so zerodb writes the block itself.
//!   Then `codex mcp get zerodb --json`, which only prints config, must show it.
//! - The SessionStart hook, a group in `hooks.SessionStart` of `hooks.json`, edited by
//!   [`crate::jsonedit`] and found by its content, as for Claude Code.
//!
//! Codex reads the MCP server's instructions, so `AGENTS.md` isn't touched. Codex keeps 1
//! approval per hook in `config.toml`. zerodb never writes those.

use super::claude::{
    add_hook, drop_hooks, edit_file, find_program, hook_args, hook_command, our_group, report, run,
    shell_line, split, Edit, Line, Part,
};
use super::{absolute_env, binary, delete, moved_home, read, write, Harness, Notes};
use crate::home::{user_home, HOME_ENV};
use serde_json::{json, Value};
use std::cell::Cell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use zerodb_tape::{Io, StdIo};

/// The variable that moves Codex's config dir.
const CODEX_ENV: &str = "CODEX_HOME";
/// How every begin line of zerodb's block starts, whatever its version.
const BEGIN: &str = "# zerodb:begin";
/// The begin line zerodb writes.
const BEGIN_LINE: &str = "# zerodb:begin v1. zerodb disconnect codex removes this block.";
/// The end line of zerodb's block.
const END: &str = "# zerodb:end";
/// The `codex` args that print zerodb's server config.
const GET: [&str; 4] = ["mcp", "get", "zerodb", "--json"];
/// A missing `hooks.json` starts as this.
const NO_HOOKS: &str = "{\n  \"hooks\": {}\n}\n";
/// The most of the hook's output Codex keeps. The block is at most 6,000 characters, so this
/// fits it whether Codex counts bytes or characters.
const CONTEXT_LIMIT: u64 = 24_000;
/// What to do after adding the hook.
const APPROVE: &str =
    "Open Codex once and approve the zerodb hook; Codex runs a new hook only after that.";
/// Why Codex may ask again for hooks after a group zerodb removed.
const POSITION: &str = "Codex keys hook approvals by position, so it may ask you to approve \
                        the SessionStart hooks after zerodb's again.";

/// Refuses a relative `CODEX_HOME`. A `codex` child inherits it, so it would read another
/// `config.toml` than zerodb writes. An empty one counts as unset.
fn check_codex_env() -> Result<(), String> {
    match std::env::var_os(CODEX_ENV) {
        Some(dir) if !dir.is_empty() && !Path::new(&dir).is_absolute() => Err(format!(
            "{CODEX_ENV} {} must be an absolute path. Nothing changed",
            Path::new(&dir).display()
        )),
        _ => Ok(()),
    }
}

/// `$CODEX_HOME` when set and absolute, else `~/.codex`.
fn codex_home() -> Result<PathBuf, String> {
    absolute_env(CODEX_ENV).map_or_else(|| Ok(user_home()?.join(".codex")), Ok)
}

/// Codex's `config.toml`, which holds the MCP servers.
pub(super) fn config_file() -> Result<PathBuf, String> {
    Ok(codex_home()?.join("config.toml"))
}

/// Codex's `hooks.json`.
fn hooks_file() -> Result<PathBuf, String> {
    Ok(codex_home()?.join("hooks.json"))
}

/// Adds the MCP block for `notes`, and the hook when `hook`. Returns the exit code: 2 for a
/// relative `CODEX_HOME`.
pub(super) fn connect(hook: bool, notes: Notes) -> u8 {
    if let Err(error) = check_codex_env() {
        eprintln!("zerodb: {error}");
        return 2;
    }
    let prepared = (|| {
        let home = moved_home(Harness::Codex)?;
        let exe = binary()?;
        let config = config_file()?;
        let warnings = split(Harness::Codex, home.as_deref());
        crate::home::ensure()?;
        Ok::<_, String>((home, exe, config, warnings))
    })();
    let (home, exe, config, mut warnings) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("zerodb: {error}");
            return 1;
        }
    };
    let home = home.as_deref();
    let mut lines = vec![set_block(&config, &exe, home, notes)];
    let mut next = Vec::new();
    if hook {
        if hooks_off(&config) {
            warnings.push(format!(
                "{} sets hooks = false in [features], so Codex runs no hooks, zerodb's \
                 included. Remove that line to load memory at chat start",
                config.display()
            ));
        }
        let group = hook_group(&exe, home);
        let (changed, moved) = (Cell::new(false), Cell::new(false));
        let add =
            |text: &str| track(text, add_hook(Harness::Codex, text, &group), &changed, &moved);
        let by_hand = format!("Add this group to hooks.SessionStart by hand: {group}");
        let parts = [Part { name: "SessionStart hook", by_hand, edit: &add }];
        let hook_lines = edit_file(hooks_file(), NO_HOOKS, &parts);
        if hook_lines.iter().all(Result::is_ok) {
            next.extend(changed.get().then_some(APPROVE));
            next.extend(moved.get().then_some(POSITION));
        }
        lines.extend(hook_lines);
    }
    let next = next.join("\n");
    report(lines, &warnings, Some(next.as_str()).filter(|next| !next.is_empty()))
}

/// Removes zerodb's MCP block and every hook group that runs zerodb's hook. Returns the exit
/// code: 2 for a relative `CODEX_HOME`.
pub(super) fn disconnect() -> u8 {
    if let Err(error) = check_codex_env() {
        eprintln!("zerodb: {error}");
        return 2;
    }
    let config = match config_file() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("zerodb: {error}");
            return 1;
        }
    };
    let (changed, moved) = (Cell::new(false), Cell::new(false));
    let drop = |text: &str| track(text, drop_hooks(Harness::Codex, text), &changed, &moved);
    let by_hand = format!(
        "Remove the groups that run zerodb{} from hooks.SessionStart by hand",
        hook_args(Harness::Codex)
    );
    let parts = [Part { name: "SessionStart hook", by_hand, edit: &drop }];
    let mut lines = vec![drop_block(&config)];
    let hook_lines = edit_file(hooks_file(), NO_HOOKS, &parts);
    let next = (moved.get() && hook_lines.iter().all(Result::is_ok)).then_some(POSITION);
    lines.extend(hook_lines);
    report(lines, &[], next)
}

/// Passes `edit` of `text` through. Sets `changed` when it changed the text, and `moved`
/// when it also removed a zerodb group that other groups followed, so they moved up.
fn track(text: &str, edit: Edit, changed: &Cell<bool>, moved: &Cell<bool>) -> Edit {
    if edit.as_ref().is_ok_and(|done| done.text != text) {
        changed.set(true);
        let root: Value = serde_json::from_str(text).unwrap_or_default();
        let groups = root["hooks"]["SessionStart"].as_array().map(Vec::as_slice);
        let groups = groups.unwrap_or_default();
        let ours = |group: &Value| our_group(Harness::Codex, group);
        let first = groups.iter().position(ours);
        moved.set(first.is_some_and(|first| !groups[first..].iter().all(ours)));
    }
    edit
}

/// The SessionStart group zerodb adds. It has no matcher, so it also runs after compaction.
fn hook_group(exe: &str, home: Option<&str>) -> Value {
    let command = hook_command(Harness::Codex, exe, home);
    json!({"hooks": [{"type": "command", "command": command, "timeout": 5,
        "statusMessage": "Loading zerodb memory", "additionalContextLimit": CONTEXT_LIMIT}]})
}

/// `text` as a TOML basic string, with `\` and `"` escaped. Control characters are refused.
fn toml_string(text: &str) -> Result<String, String> {
    if text.chars().any(char::is_control) {
        return Err(format!(
            "{text:?} holds a control character, which zerodb doesn't write to config.toml"
        ));
    }
    Ok(format!("\"{}\"", text.replace('\\', r"\\").replace('"', "\\\"")))
}

/// zerodb's block for `exe` on `home`, with `\n` line ends. With [`Notes::Auto`],
/// `memory_append` is approved, so notes are saved without asking.
fn block(exe: &str, home: Option<&str>, notes: Notes) -> Result<String, String> {
    let agent = Harness::Codex.agent();
    let mut lines = vec![
        BEGIN_LINE.to_owned(),
        "[mcp_servers.zerodb]".into(),
        format!("command = {}", toml_string(exe)?),
        format!("args = [\"mcp\", \"--agent\", \"{agent}\"]"),
        "default_tools_approval_mode = \"writes\"".into(),
    ];
    if let Some(home) = home {
        lines.push(format!("env = {{ {HOME_ENV} = {} }}", toml_string(home)?));
    }
    if notes == Notes::Auto {
        let approve =
            ["", "[mcp_servers.zerodb.tools.memory_append]", "approval_mode = \"approve\""];
        lines.extend(approve.map(String::from));
    }
    lines.push(END.into());
    Ok(lines.iter().map(|line| format!("{line}\n")).collect())
}

/// The bytes of zerodb's block in `text`, from the start of its begin line to the end of its
/// end line. An error says how the marker lines don't pair up.
fn find_block(text: &str) -> Result<Option<Range<usize>>, String> {
    let (mut begins, mut ends, mut at) = (Vec::new(), Vec::new(), 0);
    for line in text.split_inclusive('\n') {
        let marker = line.trim();
        if marker.starts_with(BEGIN) {
            begins.push(at);
        } else if marker == END {
            ends.push(at + line.len());
        }
        at += line.len();
    }
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([begin], [end]) if begin < end => Ok(Some(*begin..*end)),
        ([], _) => Err(format!("it has a line {END} without a line {BEGIN}")),
        ([_], []) => Err(format!("it has a line {BEGIN} without a line {END}")),
        _ => Err(format!(
            "its lines {BEGIN} and {END} don't pair up: each must come once, in that order"
        )),
    }
}

/// The keys and table headers of `text` outside `skip`, as dotted paths from the root, with
/// each key's value. A line scan, enough to find a table or a key: it doesn't parse
/// multi-line strings or arrays, and splits quoted keys at their dots too.
fn keys(text: &str, skip: Option<&Range<usize>>) -> Vec<(Vec<String>, Option<String>)> {
    let path = |key: &str| -> Vec<String> {
        key.split('.').map(|part| part.trim().trim_matches(['"', '\'']).to_owned()).collect()
    };
    let (mut table, mut found, mut at) = (Vec::new(), Vec::new(), 0);
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let line = line.trim();
        if skip.is_some_and(|skip| skip.contains(&start)) || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let header = header.trim_start_matches('[');
            if let Some(end) = header.find(']') {
                table = path(&header[..end]);
                found.push((table.clone(), None));
            }
        } else if let Some((key, value)) = line.split_once('=') {
            let full = table.iter().cloned().chain(path(key)).collect();
            found.push((full, Some(value.trim().to_owned())));
        }
    }
    found
}

/// Whether `text` defines a `zerodb` MCP server outside `ours`, zerodb's block.
fn foreign(text: &str, ours: Option<&Range<usize>>) -> bool {
    keys(text, ours)
        .iter()
        .any(|(path, _)| path.starts_with(&["mcp_servers".into(), "zerodb".into()]))
}

/// Whether the `config.toml` at `path` turns hooks off with `hooks = false` in `[features]`.
fn hooks_off(path: &Path) -> bool {
    let Ok(Some((bytes, _))) = read(path) else { return false };
    let wanted = ["features", "hooks"].map(String::from);
    keys(&String::from_utf8_lossy(&bytes), None).iter().any(|(key, value)| {
        let value = value.as_deref().and_then(|value| value.split('#').next());
        *key == wanted && value.is_some_and(|value| value.trim() == "false")
    })
}

/// Puts zerodb's block for `exe`, `home` and `notes` in the `config.toml` at `path`: in place
/// of an older one, else at the end, after 1 blank line. A file with CRLF line ends gets
/// CRLF. Then `codex mcp get zerodb --json` must show it, or the file is put back.
fn set_block(path: &Path, exe: &str, home: Option<&str>, notes: Notes) -> Line {
    let shown = path.display();
    let skipped = |why: String| format!("MCP entry: skipped, {why}");
    let wanted = block(exe, home, notes).map_err(skipped)?;
    let add = format!("Add this block to the end of {shown} by hand:\n{}", wanted.trim_end());
    let old = read(path).map_err(|why| skipped(format!("{shown} {why}. {add}")))?;
    let text = match &old {
        Some((bytes, _)) => std::str::from_utf8(bytes)
            .map_err(|_| skipped(format!("{shown} is not UTF-8. {add}")))?,
        None => "",
    };
    let found = find_block(text).map_err(|why| {
        skipped(format!(
            "{shown}: {why}. Nothing changed. Remove the zerodb lines there by hand, then re-run \
             zerodb connect codex"
        ))
    })?;
    if foreign(text, found.as_ref()) {
        return Err(skipped(format!(
            "{shown} has a zerodb MCP server that zerodb didn't write. zerodb doesn't edit it, \
             even with --force, and a second one would break the file. Nothing changed. Merge \
             this block into it by hand:\n{}",
            wanted.trim_end()
        )));
    }
    let eol = match text.find('\n') {
        Some(at) if text[..at].ends_with('\r') => "\r\n",
        _ => "\n",
    };
    let ours = wanted.replace('\n', eol);
    let (new, verb, by_hand) = match found {
        Some(range) if text[range.clone()] == ours => {
            return Ok(format!("MCP entry: unchanged ({shown})"));
        }
        Some(range) => {
            let new = format!("{}{ours}{}", &text[..range.start], &text[range.end..]);
            let by_hand =
                format!("Replace zerodb's block in {shown} by hand with:\n{}", wanted.trim_end());
            (new, "updated", by_hand)
        }
        None => {
            let mut new = text.to_owned();
            if !new.is_empty() {
                if !new.ends_with('\n') {
                    new.push_str(eol);
                }
                new.push_str(eol);
            }
            (new + &ours, "added", add)
        }
    };
    let backup = write(path, old.as_ref(), new.as_bytes())
        .map_err(|why| format!("MCP entry: failed, {why}. {by_hand}"))?;
    let saved = backup.as_ref().map(|backup| format!(", backup: {}", backup.display()));
    let done = format!("MCP entry: {verb} ({shown}{})", saved.unwrap_or_default());
    let Some(codex) = find_program("codex") else {
        return Ok(format!("{done}, not verified: codex is not on PATH"));
    };
    let dir = path.parent().unwrap_or(Path::new("/"));
    check(&codex, dir, exe, home)
        .map(|()| done)
        .map_err(|why| format!("MCP entry: failed, {why}. {}", restore(path, backup.as_deref())))
}

/// Checks that `codex mcp get zerodb --json`, run in `dir`, shows zerodb's command and args
/// for `exe`, and `home`. Its output is never shown, as its env may hold secrets.
fn check(codex: &Path, dir: &Path, exe: &str, home: Option<&str>) -> Result<(), String> {
    let args = GET.map(String::from);
    let line = shell_line("codex", &args);
    let mut command = Command::new(codex);
    // Not the current directory, whose project config could define another zerodb.
    command.args(&args).current_dir(dir);
    // zerodb reads an empty CODEX_HOME as unset, so `codex` must too.
    if std::env::var_os(CODEX_ENV).is_some_and(|dir| dir.is_empty()) {
        command.env_remove(CODEX_ENV);
    }
    let out = run(&mut command, &line, true)?;
    let out: Value =
        serde_json::from_slice(&out).map_err(|_| format!("{line} did not print JSON"))?;
    let server = &out["transport"];
    let shown_home = server["env"][HOME_ENV].as_str().filter(|home| !home.is_empty());
    let agent = Harness::Codex.agent();
    if server["command"] == exe
        && server["args"] == json!(["mcp", "--agent", agent])
        && shown_home.map(Path::new) == home.map(Path::new)
    {
        return Ok(());
    }
    let on = home.map(|home| format!(" with {HOME_ENV} {home}")).unwrap_or_default();
    Err(format!("{line} does not show {} mcp --agent {agent}{on}", json!(exe)))
}

/// Puts `path` back as it was before zerodb's write: renames `backup` over it, or deletes it
/// when zerodb made it. Returns what to print.
fn restore(path: &Path, backup: Option<&Path>) -> String {
    let shown = path.display();
    let parent = path.parent().unwrap_or(Path::new("/"));
    let undone = match backup {
        Some(backup) => std::fs::rename(backup, path)
            .map_err(|error| format!("{shown}: cannot restore {}: {error}", backup.display()))
            .and_then(|()| {
                StdIo::new()
                    .sync_dir(parent)
                    .map_err(|error| format!("{}: cannot sync: {error}", parent.display()))
            }),
        None => delete(path),
    };
    let again = "Check what codex mcp get zerodb --json shows, then re-run zerodb connect codex";
    match (undone, backup) {
        (Ok(()), Some(_)) => format!("{shown} is back as it was. {again}"),
        (Ok(()), None) => format!("{shown} is deleted again, as zerodb made it. {again}"),
        (Err(why), Some(backup)) => format!(
            "{why}. zerodb's block is still in {shown}. The file as it was is in {}",
            backup.display()
        ),
        (Err(why), None) => {
            format!("{why}. zerodb made {shown}, and its block is still there. Delete it by hand")
        }
    }
}

/// Removes zerodb's block from the `config.toml` at `path`, with the blank line `connect`
/// put before it.
fn drop_block(path: &Path) -> Line {
    let shown = path.display();
    let by_hand = format!("Remove the lines from {BEGIN} to {END} in {shown} by hand");
    let old = read(path).map_err(|why| format!("MCP entry: skipped, {shown} {why}. {by_hand}"))?;
    let Some((bytes, _)) = &old else { return Ok(format!("MCP entry: not found ({shown})")) };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| format!("MCP entry: skipped, {shown} is not UTF-8. {by_hand}"))?;
    let range = match find_block(text) {
        Ok(Some(range)) => range,
        Ok(None) if foreign(text, None) => {
            return Err(format!(
                "MCP entry: skipped, {shown} has a zerodb MCP server that zerodb didn't write. \
                 Remove it by hand to disconnect it"
            ));
        }
        Ok(None) => return Ok(format!("MCP entry: not found ({shown})")),
        Err(why) => {
            return Err(format!(
                "MCP entry: skipped, {shown}: {why}. Nothing changed. Remove the zerodb lines \
                 there by hand"
            ));
        }
    };
    let before = &text[..range.start];
    // The blank line `connect` put before the block goes too, so its line end stays.
    let blank = ["\r\n\r\n", "\n\n"].into_iter().find(|blank| before.ends_with(blank));
    let before = blank.map_or(before, |blank| &before[..before.len() - blank.len() / 2]);
    let new = format!("{before}{}", &text[range.end..]);
    let backup = write(path, old.as_ref(), new.as_bytes())
        .map_err(|why| format!("MCP entry: failed, {why}. {by_hand}"))?;
    let saved = backup.map(|backup| format!(", backup: {}", backup.display()));
    Ok(format!("MCP entry: removed ({shown}{})", saved.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{find_block, foreign, keys, toml_string};

    #[test]
    fn toml_strings_escape_and_refuse_control_characters() {
        assert_eq!(toml_string(r#"/a "b" \c"#).unwrap(), r#""/a \"b\" \\c""#);
        for bad in ["/a\nb", "/a\tb", "/a\u{7f}b", "/a\u{85}b"] {
            assert!(toml_string(bad).unwrap_err().contains("control character"), "{bad:?}");
        }
    }

    #[test]
    fn the_line_scan_finds_tables_and_keys() {
        let text = "model = \"o3\"\nmcp_servers.zerodb.command = \"x\"\n[mcp_servers]\n\
                    zerodb = { command = \"x\" }\n[ mcp_servers . \"zerodb\" .env ]\n";
        let found: Vec<_> = keys(text, None).into_iter().map(|(path, _)| path.join(".")).collect();
        let wanted = [
            "model",
            "mcp_servers.zerodb.command",
            "mcp_servers",
            "mcp_servers.zerodb",
            "mcp_servers.zerodb.env",
        ];
        assert_eq!(found, wanted);
        assert!(!foreign(
            "[mcp_servers.zerodbx]\n[mcp_server.zerodb]\n# [mcp_servers.zerodb]\n",
            None
        ));
        let text = "a = 1\n# zerodb:begin v1\n[mcp_servers.zerodb]\n# zerodb:end\n";
        let ours = find_block(text).unwrap().unwrap();
        assert_eq!(&text[ours.clone()], &text[6..]);
        assert!(!foreign(text, Some(&ours)) && foreign(text, None));
    }
}
