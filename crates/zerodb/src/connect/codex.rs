//! Codex's 3 parts, in `$CODEX_HOME`, else `~/.codex`:
//!
//! - The MCP entry with its tool approvals: a block between 2 marker lines at the end of
//!   `config.toml`. `codex mcp add` can't set approvals, so zerodb writes the block itself.
//!   Then `codex mcp get zerodb --json`, which only prints config, must show it.
//! - The SessionStart hook, a group in `hooks.SessionStart` of `hooks.json`, edited by
//!   [`crate::jsonedit`] and found by its content, as for Claude Code.
//! - The guide: the MCP server's instructions as a block in `AGENTS.md`, or in
//!   `AGENTS.override.md` when Codex reads that instead. Codex 0.147 didn't pass MCP server
//!   instructions to the model in `codex exec`, so the guide goes where Codex reads the
//!   user's own. See [`guide`].
//!
//! Codex keeps 1 approval per hook in `config.toml`. zerodb never writes those.

use super::claude::{
    add_hook, drop_hooks, edit_file, find_program, hook_args, hook_command, our_group, run,
    shell_line, split, Edit, Part,
};
use super::{
    absolute_env, add_or_update, binary, delete, moved_home, planned, read, write, Harness, Line,
    Mode, Notes, Report, State,
};
use crate::home::{user_home, HOME_ENV};
use serde_json::{json, Value};
use std::cell::Cell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use zerodb_tape::{Io, StdIo};

mod guide;
mod scan;
use scan::{scan, Entry, Item};

/// The variable that moves Codex's config dir.
const CODEX_ENV: &str = "CODEX_HOME";
/// The begin line zerodb writes.
const BEGIN_LINE: &str = "# zerodb:begin v1. zerodb disconnect codex removes this block.";
/// The begin line zerodb writes after adding the line end the file lacked, so `disconnect`
/// removes that line end too.
const BEGIN_ADDED: &str = "# zerodb:begin v1. zerodb disconnect codex removes this block and \
                           the line end it added above.";
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
pub(crate) const APPROVE: &str =
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
pub(super) fn codex_home() -> Result<PathBuf, String> {
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

/// Adds the MCP block for `notes`, the hook when `hook`, and the guide. It stops with exit
/// code 2 for a relative `CODEX_HOME`. In Plan mode, it writes nothing and runs no `codex`.
pub(super) fn connect(hook: bool, notes: Notes, mode: Mode) -> Report {
    if let Err(error) = check_codex_env() {
        return Report::stopped(2, error);
    }
    let prepared = (|| {
        let home = moved_home(Harness::Codex)?;
        let exe = binary()?;
        let dir = codex_home()?;
        let config = dir.join("config.toml");
        let warnings = split(Harness::Codex, home.as_deref());
        match mode {
            Mode::Apply => drop(crate::home::ensure()?),
            Mode::Plan => crate::home::check_existing()?,
        }
        Ok::<_, String>((home, exe, dir, config, warnings))
    })();
    let (home, exe, dir, config, mut warnings) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Report::stopped(1, error),
    };
    let home = home.as_deref();
    let mut lines = vec![set_block(&config, &exe, home, notes, mode).unwrap_or_else(|line| line)];
    let (mut next, mut hook_state, off) = (Vec::new(), None, hook && hooks_off(&config));
    if hook {
        if off {
            warnings.push(format!(
                "{} sets hooks = false in [features], so Codex runs no hooks, zerodb's \
                 included. Remove that line to load memory at chat start",
                config.display()
            ));
        }
        let group = hook_group(&exe, home);
        let (tracked, hooks) = (Tracked::default(), hooks_file());
        let add = |text: &str| track(text, add_hook(Harness::Codex, text, &group), &tracked);
        let by_hand = format!("Add this group to hooks.SessionStart by hand: {group}");
        let parts = [Part { name: "SessionStart hook", by_hand, edit: &add }];
        // 1 part, so 1 line.
        let hook_lines = edit_file(hooks.clone(), NO_HOOKS, &parts, mode);
        hook_state = hook_lines.first().map(|line| line.state);
        lines.extend(hook_lines);
        // In Plan mode nothing was saved, so this finds nothing. With hooks off, there is
        // nothing to approve.
        if let Some(moved) = saved(&hooks, &tracked) {
            next.extend((!off).then(|| APPROVE.to_owned()));
            next.extend(moved.then(|| POSITION.to_owned()));
        }
    }
    let (guide, too_big) = guide::add(&dir, mode);
    lines.push(guide);
    warnings.extend(too_big);
    Report { lines, warnings, next, stopped: None, hook: hook_state, hooks_off: off }
}

/// Checks that `codex mcp get zerodb --json`, which only reads config, shows zerodb's entry
/// for this binary and home. `Ok` without running anything when `codex` is not on `PATH`.
pub(super) fn verify() -> Result<(), String> {
    check_codex_env()?;
    let home = moved_home(Harness::Codex)?;
    let (exe, config) = (binary()?, config_file()?);
    let Some(codex) = find_program("codex") else { return Ok(()) };
    check(&codex, config.parent().unwrap_or(Path::new("/")), &exe, home.as_deref())
}

/// Removes zerodb's MCP block, every hook group that runs zerodb's hook, and the guide's
/// block. Returns the exit code: 2 for a relative `CODEX_HOME`.
pub(super) fn disconnect() -> u8 {
    if let Err(error) = check_codex_env() {
        eprintln!("zerodb: {error}");
        return 2;
    }
    let dir = match codex_home() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("zerodb: {error}");
            return 1;
        }
    };
    let config = dir.join("config.toml");
    let (tracked, hooks) = (Tracked::default(), hooks_file());
    let drop = |text: &str| track(text, drop_hooks(Harness::Codex, text), &tracked);
    let by_hand = format!(
        "Remove the groups that run zerodb{} from hooks.SessionStart by hand",
        hook_args(Harness::Codex)
    );
    let parts = [Part { name: "SessionStart hook", by_hand, edit: &drop }];
    let mut lines = vec![drop_block(&config).unwrap_or_else(|line| line)];
    lines.extend(edit_file(hooks.clone(), NO_HOOKS, &parts, Mode::Apply));
    lines.extend(guide::remove(&dir));
    // A removal that left another zerodb handler to do by hand still moved the groups after it.
    let next = saved(&hooks, &tracked).filter(|moved| *moved).map(|_| POSITION.to_owned());
    Report { lines, next: next.into_iter().collect(), ..Report::default() }.print()
}

/// A hook edit that changed `hooks.json`: its new text, and whether it removed a zerodb group
/// that other groups followed, so they moved up.
type Tracked = Cell<Option<(String, bool)>>;

/// Passes `edit` of `text` through, and records it in `tracked` when it changed the text.
fn track(text: &str, edit: Edit, tracked: &Tracked) -> Edit {
    if let Some(done) = edit.as_ref().ok().filter(|done| done.text != text) {
        let root: Value = serde_json::from_str(text).unwrap_or_default();
        let groups = root["hooks"]["SessionStart"].as_array().map(Vec::as_slice);
        let groups = groups.unwrap_or_default();
        let ours = |group: &Value| our_group(Harness::Codex, group);
        let first = groups.iter().position(ours);
        let moved = first.is_some_and(|first| !groups[first..].iter().all(ours));
        tracked.set(Some((done.text.clone(), moved)));
    }
    edit
}

/// Whether the groups after a removed zerodb group moved up, when the edit in `tracked` was
/// saved: the file at `path` now holds its text. `None` when nothing was saved. The part's
/// line may still say to do more by hand.
fn saved(path: &Result<PathBuf, String>, tracked: &Tracked) -> Option<bool> {
    let (text, moved) = tracked.take()?;
    let path = path.as_ref().ok()?;
    matches!(read(path), Ok(Some((bytes, _))) if bytes == text.as_bytes()).then_some(moved)
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

/// zerodb's block in a file: the bytes from the start of its begin line to the end of its end
/// line, and which of them are zerodb's.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    /// From the start of the begin line to after the end line.
    span: Range<usize>,
    /// zerodb's bytes in the span, in order: the 2 marker lines, each `mcp_servers.zerodb`
    /// header and the key lines of its table, and the blank lines between 2 of those. Codex
    /// inserts tables it adds before a comment at the end of the file, so other tables can sit
    /// inside the markers. They, comments, and the blank lines around them aren't zerodb's.
    ours: Vec<Range<usize>>,
}

impl Block {
    /// Whether `entry` lies in zerodb's bytes.
    fn holds(&self, entry: &Entry) -> bool {
        self.ours.iter().any(|ours| ours.contains(&entry.span.start))
    }
}

/// Whether `path` is zerodb's server table, or a table under it.
fn zerodb_path(path: &[String]) -> bool {
    matches!(path, [servers, zerodb, ..] if servers == "mcp_servers" && zerodb == "zerodb")
}

/// zerodb's block in a file scanned as `entries`. Markers are comment lines that are exactly
/// a begin line zerodb writes, or [`END`]. An error says how they don't pair up.
fn find_block(entries: &[Entry]) -> Result<Option<Block>, String> {
    let (mut begins, mut ends) = (Vec::new(), Vec::new());
    for (at, entry) in entries.iter().enumerate() {
        let Item::Comment(comment) = &entry.item else { continue };
        match comment.trim() {
            BEGIN_LINE | BEGIN_ADDED => begins.push(at),
            END => ends.push(at),
            _ => {}
        }
    }
    let (begin, end) = match (begins.as_slice(), ends.as_slice()) {
        ([], []) => return Ok(None),
        ([begin], [end]) if begin < end => (*begin, *end),
        ([], _) => return Err(format!("it has a line {END} without a zerodb begin line")),
        ([_], []) => return Err(format!("it has a zerodb begin line without a line {END}")),
        _ => {
            return Err(format!(
                "its zerodb begin lines and lines {END} don't pair up: each must come once, in \
                 that order"
            ))
        }
    };
    // The markers, each zerodb header and the keys of its table are zerodb's. A comment never
    // is, and a key goes with the table it sits in.
    let all = &entries[begin..=end];
    let mut section = false;
    let mine: Vec<bool> = (all.iter().enumerate())
        .map(|(at, entry)| match &entry.item {
            _ if at == 0 || at == all.len() - 1 => true,
            Item::Table(path) => {
                section = zerodb_path(path);
                section
            }
            Item::Key(..) => section,
            Item::Comment(_) => false,
        })
        .collect();
    // Runs of zerodb's lines, with the blank lines between them.
    let mut ours: Vec<Range<usize>> = Vec::new();
    for (at, entry) in all.iter().enumerate().filter(|(at, _)| mine[*at]) {
        match ours.last_mut() {
            Some(last) if mine[at - 1] => last.end = entry.span.end,
            _ => ours.push(entry.span.clone()),
        }
    }
    Ok(Some(Block { span: entries[begin].span.start..entries[end].span.end, ours }))
}

/// `text` without zerodb's bytes in `block`, and without the blank line `connect` put before
/// the block. When nothing is left after the block, the line end `connect` added before it
/// goes too, so the file is back as it was. Every other byte stays.
fn without(text: &str, block: &Block) -> String {
    let head = &text[..block.span.start];
    let blank = ["\r\n\r\n", "\n\n"].into_iter().find(|blank| head.ends_with(blank));
    let mut head = blank.map_or(head, |blank| &head[..head.len() - blank.len() / 2]);
    let (mut kept, mut at) = (String::new(), block.span.start);
    for ours in &block.ours {
        kept.push_str(&text[at..ours.start]);
        at = ours.end;
    }
    let tail = &text[block.span.end..];
    let begin = text[block.span.start..].split('\n').next().unwrap_or_default();
    if added_end(begin) && kept.is_empty() && tail.is_empty() {
        head = head.strip_suffix(if begin.ends_with('\r') { "\r\n" } else { "\n" }).unwrap_or(head);
    }
    format!("{head}{kept}{tail}")
}

/// Why a block isn't edited when other `mcp_servers.zerodb` settings sit outside its own
/// lines, after the file's path. Changing only the block would leave part of a server.
const OUTSIDE: &str = "has mcp_servers.zerodb settings besides zerodb's own lines in its \
                       block, and zerodb doesn't edit them";
/// How to remove zerodb's lines from the file at `shown` by hand. Codex may have put other
/// tables between the markers, so they must stay.
fn remove_ours(shown: impl std::fmt::Display) -> String {
    format!(
        "Remove the zerodb marker lines and the [mcp_servers.zerodb] and \
         [mcp_servers.zerodb.*] tables from {shown} by hand, and keep anything else between \
         the markers"
    )
}

/// What to do then. Inside the block they would count as zerodb's, and a rewrite drops them.
const TIDY: &str = "Remove them by hand, then re-run the command";

/// What outside zerodb's block keeps zerodb from adding it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clash {
    /// A `zerodb` MCP server zerodb didn't write.
    Server,
    /// `mcp_servers = { ... }` at the root. zerodb doesn't read inside it, so it can't tell
    /// whether it holds `zerodb`, and a `[mcp_servers.zerodb]` table would break the file.
    Inline,
}

impl Clash {
    /// Why connect refuses, after the file's path.
    fn why(self) -> &'static str {
        match self {
            Self::Server => {
                "has a zerodb MCP server that zerodb didn't write. zerodb doesn't edit it, even \
                 with --force, and adding the block would break the file"
            }
            Self::Inline => {
                "sets mcp_servers as an inline table, so zerodb can't tell whether it holds \
                 zerodb, and adding the block would break the file"
            }
        }
    }
}

/// What in a file scanned as `entries`, outside zerodb's bytes in `ours`, clashes with
/// zerodb's block. A `zerodb` server counts as a table, as a key under `[mcp_servers]`, or as
/// a dotted key, by the decoded key path.
fn clash(entries: &[Entry], ours: Option<&Block>) -> Option<Clash> {
    let outside = entries.iter().filter(|entry| ours.is_none_or(|ours| !ours.holds(entry)));
    let mut found = None;
    for entry in outside {
        let (Item::Table(path) | Item::Key(path, _)) = &entry.item else { continue };
        match path.as_slice() {
            path if zerodb_path(path) => return Some(Clash::Server),
            [servers] if servers == "mcp_servers" && matches!(entry.item, Item::Key(..)) => {
                found = Some(Clash::Inline);
            }
            _ => {}
        }
    }
    found
}

/// Whether `block`, zerodb's block or its begin line, starts with [`BEGIN_ADDED`].
fn added_end(block: &str) -> bool {
    block.lines().next().is_some_and(|line| line.trim() == BEGIN_ADDED)
}

/// Whether the `config.toml` at `path` turns hooks off with `hooks = false` in `[features]`.
fn hooks_off(path: &Path) -> bool {
    let Ok(Some((bytes, _))) = read(path) else { return false };
    let entries = std::str::from_utf8(&bytes).map_err(|_| String::new()).and_then(scan);
    entries.unwrap_or_default().iter().any(|entry| match &entry.item {
        Item::Key(path, value) => *path == ["features", "hooks"] && value == "false",
        _ => false,
    })
}

/// Puts zerodb's block for `exe`, `home` and `notes` at the end of the `config.toml` at
/// `path`, after 1 blank line. An older block's own bytes go first, and every other byte in
/// it stays where it is. A block that is already right, with nothing else inside it, stays
/// too. A file with CRLF line ends gets CRLF. A file without a final line end gets one, and the block says so. Then
/// `codex mcp get zerodb --json` must show it, or the file is put back. In Plan mode, it
/// writes nothing and runs no `codex`. `Err` is the line of a skipped or failed part.
fn set_block(
    path: &Path,
    exe: &str,
    home: Option<&str>,
    notes: Notes,
    mode: Mode,
) -> Result<Line, Line> {
    let shown = path.display();
    let skipped = |why: String| Line::new(State::Skipped, format!("MCP entry: skipped, {why}"));
    let wanted = block(exe, home, notes).map_err(skipped)?;
    let add = format!("Add this block to the end of {shown} by hand:\n{}", wanted.trim_end());
    let old = read(path).map_err(|why| skipped(format!("{shown} {why}. {add}")))?;
    let text = match &old {
        Some((bytes, _)) => std::str::from_utf8(bytes)
            .map_err(|_| skipped(format!("{shown} is not UTF-8. {add}")))?,
        None => "",
    };
    let entries = scan(text).map_err(|why| {
        skipped(format!("{shown} isn't TOML zerodb can read: {why}. Nothing changed. {add}"))
    })?;
    let found = find_block(&entries).map_err(|why| {
        skipped(format!(
            "{shown}: {why}. Nothing changed. {}, then re-run zerodb connect codex",
            remove_ours(&shown)
        ))
    })?;
    if found.is_some() && clash(&entries, found.as_ref()) == Some(Clash::Server) {
        return Err(skipped(format!("{shown} {OUTSIDE}. Nothing changed. {TIDY}")));
    }
    if let Some(clash) = clash(&entries, found.as_ref()) {
        return Err(skipped(format!(
            "{shown} {}. Nothing changed. Merge this block into it by hand:\n{}",
            clash.why(),
            wanted.trim_end()
        )));
    }
    let eol = match text.find('\n') {
        Some(at) if text[..at].ends_with('\r') => "\r\n",
        _ => "\n",
    };
    // The block says whether `connect` added the line end before it.
    let ours = |added: bool| {
        let begin = if added { BEGIN_ADDED } else { BEGIN_LINE };
        wanted.replacen(BEGIN_LINE, begin, 1).replace('\n', eol)
    };
    let (mut new, verb, by_hand) = match found {
        Some(block) if text[block.span.clone()] == ours(added_end(&text[block.span.clone()])) => {
            return Ok(match mode {
                Mode::Plan => planned(path, "MCP entry", State::Unchanged),
                Mode::Apply => {
                    Line::new(State::Unchanged, format!("MCP entry: unchanged ({shown})"))
                }
            });
        }
        Some(block) => {
            let by_hand = format!(
                "{}, then add this block at its end:\n{}",
                remove_ours(&shown),
                wanted.trim_end()
            );
            (without(text, &block), "updated", by_hand)
        }
        None => (text.to_owned(), "added", add),
    };
    let state = add_or_update(verb == "updated");
    if mode == Mode::Plan {
        return Ok(planned(path, "MCP entry", state));
    }
    let added = !new.is_empty() && !new.ends_with('\n');
    if !new.is_empty() {
        if added {
            new.push_str(eol);
        }
        new.push_str(eol);
    }
    new.push_str(&ours(added));
    let failed = |why: String| Line::new(State::Failed, format!("MCP entry: failed, {why}"));
    let backup = write(path, old.as_ref(), new.as_bytes())
        .map_err(|why| failed(format!("{why}. {by_hand}")))?;
    let saved = backup.as_ref().map(|backup| format!(", backup: {}", backup.display()));
    let done = format!("MCP entry: {verb} ({shown}{})", saved.unwrap_or_default());
    let Some(codex) = find_program("codex") else {
        return Ok(Line::new(state, format!("{done}, not verified: codex is not on PATH")));
    };
    let dir = path.parent().unwrap_or(Path::new("/"));
    check(&codex, dir, exe, home).map(|()| Line::new(state, done)).map_err(|why| {
        let undone = restore(path, backup.as_deref(), new.as_bytes());
        failed(format!("{why}. {undone}"))
    })
}

/// Checks that `codex mcp get zerodb --json`, run in `dir`, shows zerodb's command and args
/// for `exe`, and `home`. Its output is never shown, as its env and its errors may hold
/// secrets from the config.
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
    let out = run(&mut command, &line, true)
        .map_err(|why| format!("{why}; run it yourself to see why"))?;
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

/// Puts `path` back as it was before zerodb wrote `written` there: renames `backup` over
/// it, or deletes it when zerodb made it. A file that no longer holds `written` was changed
/// since, say by Codex, so it stays as it is. Returns what to print.
fn restore(path: &Path, backup: Option<&Path>, written: &[u8]) -> String {
    let shown = path.display();
    let parent = path.parent().unwrap_or(Path::new("/"));
    if !matches!(read(path), Ok(Some((bytes, _))) if bytes == written) {
        return match backup {
            Some(backup) => format!(
                "{shown} changed while zerodb was checking it, so zerodb didn't put it back. The \
                 file as it was before zerodb wrote it is in {}, to restore by hand",
                backup.display()
            ),
            None => format!(
                "{shown} changed while zerodb was checking it, so zerodb didn't delete it, \
                 though zerodb made it. {}",
                remove_ours(&shown)
            ),
        };
    }
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
    let again = "Re-run zerodb connect codex once that is fixed";
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

/// Removes zerodb's bytes in its block from the `config.toml` at `path`, by [`without`].
/// `Err` is the line of a skipped or failed part.
fn drop_block(path: &Path) -> Result<Line, Line> {
    let shown = path.display();
    let by_hand = remove_ours(&shown);
    let skipped = |why: String| Line::new(State::Skipped, format!("MCP entry: skipped, {why}"));
    let not_found = || Line::new(State::Unchanged, format!("MCP entry: not found ({shown})"));
    let old = read(path).map_err(|why| skipped(format!("{shown} {why}. {by_hand}")))?;
    let Some((bytes, _)) = &old else { return Ok(not_found()) };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| skipped(format!("{shown} is not UTF-8. {by_hand}")))?;
    let entries = scan(text)
        .map_err(|why| skipped(format!("{shown} isn't TOML zerodb can read: {why}. {by_hand}")))?;
    let block = match find_block(&entries) {
        // Removing zerodb's lines would leave the others as part of a server.
        Ok(Some(block)) if clash(&entries, Some(&block)) == Some(Clash::Server) => {
            return Err(skipped(format!("{shown} {OUTSIDE}. Nothing changed. {TIDY}")));
        }
        Ok(Some(block)) => block,
        Ok(None) => {
            return match clash(&entries, None) {
                Some(Clash::Server) => Err(skipped(format!(
                    "{shown} has a zerodb MCP server that zerodb didn't write. Remove it by hand \
                     to disconnect it"
                ))),
                Some(Clash::Inline) => Err(skipped(format!(
                    "{shown} sets mcp_servers as an inline table, so zerodb can't tell whether it \
                     holds zerodb. If it does, remove zerodb there by hand"
                ))),
                None => Ok(not_found()),
            };
        }
        Err(why) => {
            return Err(skipped(format!(
                "{shown}: {why}. Nothing changed. {}",
                remove_ours(&shown)
            )));
        }
    };
    let new = without(text, &block);
    let backup = write(path, old.as_ref(), new.as_bytes())
        .map_err(|why| Line::new(State::Failed, format!("MCP entry: failed, {why}. {by_hand}")))?;
    let saved = backup.map(|backup| format!(", backup: {}", backup.display()));
    Ok(Line::new(
        State::Removed,
        format!("MCP entry: removed ({shown}{})", saved.unwrap_or_default()),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{clash, find_block, scan, toml_string, without, Clash};

    #[test]
    fn toml_strings_escape_and_refuse_control_characters() {
        assert_eq!(toml_string(r#"/a "b" \c"#).unwrap(), r#""/a \"b\" \\c""#);
        for bad in ["/a\nb", "/a\tb", "/a\u{7f}b", "/a\u{85}b"] {
            assert!(toml_string(bad).unwrap_err().contains("control character"), "{bad:?}");
        }
    }

    #[test]
    fn clashes_and_markers_go_by_the_decoded_structure() {
        let server = Some(Clash::Server);
        let rows = [
            ("[mcp_servers.zerodb]\n", server),
            ("[ mcp_servers . 'zerodb' . env ]\n", server),
            ("[mcp_servers.\"zero\\u0064b\"]\n", server),
            ("[mcp_servers]\nzerodb = { command = \"x\" }\n", server),
            ("mcp_servers.zerodb.command = \"x\"\n", server),
            ("[[mcp_servers.zerodb.list]]\n", server),
            ("[mcp_servers.\"zerodb.proxy\"]\n", None),
            ("[mcp_servers.zerodbx]\n[mcp_server.zerodb]\n# [mcp_servers.zerodb]\n", None),
            ("s = \"\"\"\n[mcp_servers.zerodb]\n\"\"\"\n", None),
            ("a = [\n[\"mcp_servers.zerodb\"],\n]\n", None),
            ("[profiles.x]\nmcp_servers = {}\n", None),
            (" mcp_servers= { zerodb = {} }\n", Some(Clash::Inline)),
        ];
        for (text, wanted) in rows {
            assert!(clash(&scan(text).unwrap(), None) == wanted, "{text:?}");
        }
        let begin = "# zerodb:begin v1. zerodb disconnect codex removes this block.";
        let text = format!("a = 1\n {begin} \n[mcp_servers.zerodb]\n# zerodb:end\n");
        let entries = scan(&text).unwrap();
        let ours = find_block(&entries).unwrap().unwrap();
        assert_eq!(&text[ours.span.clone()], &text[6..]);
        assert!(clash(&entries, Some(&ours)).is_none() && clash(&entries, None) == server);
        let not_markers = format!("# zerodb:beginner\n# zerodb:end.\ns = '''\n{begin}\n'''\n");
        assert_eq!(find_block(&scan(&not_markers).unwrap()), Ok(None));
    }

    #[test]
    fn only_zerodb_bytes_in_the_block_go() {
        let begin = "# zerodb:begin v1. zerodb disconnect codex removes this block.";
        // As toml_edit writes a table it adds: after the last table, before the end comment.
        // Comments, and the blank lines around them, aren't zerodb's, even inside its table.
        let text = format!(
            "a = 1\n\n{begin}\nx.y = 2\n[mcp_servers.zerodb]\ncommand = \"z\"\n# mine\nargs = []\n\n\
             [mcp_servers.zerodb.tools.t]\nk = 1\n# keep me too\n\n[projects.\"/r\"]\n\
             trust_level = \"trusted\"\n\n[mcp_servers.zerodb.env]\nA = \"b\"\n# zerodb:end\n\
             [after]\n"
        );
        let block = find_block(&scan(&text).unwrap()).unwrap().unwrap();
        let ours: Vec<&str> = block.ours.iter().map(|ours| &text[ours.clone()]).collect();
        let wanted = [
            &format!("{begin}\n")[..],
            "[mcp_servers.zerodb]\ncommand = \"z\"\n",
            "args = []\n\n[mcp_servers.zerodb.tools.t]\nk = 1\n",
            "[mcp_servers.zerodb.env]\nA = \"b\"\n# zerodb:end\n",
        ];
        assert_eq!(ours, wanted);
        let kept = "a = 1\nx.y = 2\n# mine\n# keep me too\n\n[projects.\"/r\"]\n\
                    trust_level = \"trusted\"\n\n[after]\n";
        assert_eq!(without(&text, &block), kept);
        // The line end connect added before the block goes only when nothing follows it.
        let added = "# zerodb:begin v1. zerodb disconnect codex removes this block and the line \
                     end it added above.";
        let text = format!("a = 1\n\n{added}\n[mcp_servers.zerodb]\n# zerodb:end\n");
        assert_eq!(without(&text, &find_block(&scan(&text).unwrap()).unwrap().unwrap()), "a = 1");
        let text = format!("a = 1\n\n{added}\n[mcp_servers.zerodb]\n[p]\n# zerodb:end\n");
        assert_eq!(
            without(&text, &find_block(&scan(&text).unwrap()).unwrap().unwrap()),
            "a = 1\n[p]\n"
        );
    }
}
