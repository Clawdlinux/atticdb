//! `atticdb connect` and `atticdb disconnect`: add or remove AtticDB's entry in a harness's
//! user MCP config, and 2 Copilot files VS Code and Copilot CLI share: the `sessionStart`
//! hook and the instructions file. Claude Code, Codex and Gemini CLI have their own parts, in
//! [`claude`], [`codex`] and [`gemini`]. Antigravity and Claude Desktop get only their entry,
//! by [`plain`]. The entry and the hook run the binary on the home, so they hold no token. The
//! only paths they hold are the binary's and, under `ATTICDB_HOME`, the home's.

mod claude;
mod codex;
mod gemini;
mod guide;
mod plain;

pub use claude::Notes;
/// What to do after Codex's hook is added.
pub(crate) use codex::APPROVE as CODEX_APPROVE;
/// What setup and doctor say about Codex on Windows, where it gets only its MCP entry.
pub(crate) const CODEX_WINDOWS: &str = "Codex on Windows: MCP only, hooks wait for #142";

/// What a part did, or in [`Mode::Plan`] would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// It was added, or would be.
    Added,
    /// It was updated, or would be.
    Updated,
    /// It was already in place, or there was nothing to do.
    Unchanged,
    /// The user's own file does its job, so AtticDB's was not written.
    Kept,
    /// It was removed.
    Removed,
    /// It could not be applied, and the line says what to do by hand.
    Skipped,
    /// Applying it failed.
    Failed,
}

impl State {
    /// Whether the part is as it should be: not skipped and not failed.
    pub fn ok(self) -> bool {
        !matches!(self, Self::Skipped | Self::Failed)
    }

    /// Whether, in [`Mode::Plan`], the part needs nothing: it is unchanged, or the user's own
    /// file is kept.
    pub fn in_place(self) -> bool {
        matches!(self, Self::Unchanged | Self::Kept)
    }
}

/// A part's line to print, with what it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// What it did, or would do.
    pub state: State,
    /// The line, as printed.
    pub text: String,
}

impl Line {
    /// A line that says `text`.
    pub(crate) fn new(state: State, text: impl Into<String>) -> Self {
        Self { state, text: text.into() }
    }
}

/// Whether connecting writes its parts, or only says what it would write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Write each part, as `atticdb connect` does.
    Apply,
    /// Write nothing, and run no command that changes anything. Reads are allowed. Each part
    /// gets the line [`planned`] makes, or the line Apply would print for a skipped part.
    Plan,
}

/// A part's line in [`Mode::Plan`]: `<file>: add the <part>` for [`State::Added`],
/// `<file>: update the <part>` for [`State::Updated`], else `<file>: <part> unchanged`.
fn planned(file: &Path, part: &str, state: State) -> Line {
    let file = file.display();
    let text = match state {
        State::Added => format!("{file}: add the {part}"),
        State::Updated => format!("{file}: update the {part}"),
        _ => format!("{file}: {part} unchanged"),
    };
    Line::new(state, text)
}

/// [`State::Added`] when nothing was there, else [`State::Updated`].
fn add_or_update(existed: bool) -> State {
    if existed {
        State::Updated
    } else {
        State::Added
    }
}

/// How to connect Claude Code, Codex and Gemini CLI. VS Code and Copilot CLI always get the
/// hook, and have no allow rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Replace an entry that differs. Codex never edits a server AtticDB didn't write.
    pub force: bool,
    /// Add the SessionStart hook, and for Claude Code the PostCompact hook too.
    pub hook: bool,
    /// Whether notes are saved without asking.
    pub notes: Notes,
}

/// What connecting did, or in [`Mode::Plan`] would do.
#[derive(Debug, Default)]
pub struct Report {
    /// 1 line per file and part, in order.
    pub lines: Vec<Line>,
    /// The exit code and the error that stopped it before or between parts.
    pub stopped: Option<(u8, String)>,
    /// Notes for stderr, such as another harness on another home.
    pub warnings: Vec<String>,
    /// What to do next, printed last. Empty in Plan mode.
    pub next: Vec<String>,
    /// What the SessionStart hook part did: for Claude Code, Codex and Gemini CLI when it was
    /// asked for, and the shared sessionStart hook for VS Code and Copilot CLI.
    pub hook: Option<State>,
    /// What the PostCompact hook part did: for Claude Code when the hooks were asked for.
    pub post_compact: Option<State>,
    /// Whether the harness's config turns hooks off, so its hook never runs. Codex only.
    pub hooks_off: bool,
}

impl Report {
    /// A report of an error that stopped connecting with exit code `code`.
    fn stopped(code: u8, error: String) -> Self {
        Self { stopped: Some((code, error)), ..Self::default() }
    }

    /// Prints the lines in order, then the error or the warnings on stderr, then what to do
    /// next. Returns the exit code: the error's, else 1 when a part was skipped or failed.
    pub fn print(&self) -> u8 {
        for line in &self.lines {
            println!("{}", line.text);
        }
        if let Some((code, error)) = &self.stopped {
            eprintln!("atticdb: {error}");
            return *code;
        }
        for warning in &self.warnings {
            eprintln!("atticdb: {warning}");
        }
        for next in &self.next {
            println!("{next}");
        }
        u8::from(self.skipped())
    }

    /// Whether it stopped, or a part was skipped or failed.
    pub fn skipped(&self) -> bool {
        self.stopped.is_some() || self.lines.iter().any(|line| !line.state.ok())
    }
}

use crate::home::{user_home, HOME_ENV};
use crate::jsonedit;
#[cfg(not(unix))]
use crate::platform::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use atticdb_tape::{Io, StdIo};
use serde_json::{json, Map, Value};
#[cfg(unix)]
use std::fs::Permissions;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A harness whose user MCP config AtticDB can edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    /// VS Code's `mcp.json`, used by its Copilot chat.
    Vscode,
    /// Copilot CLI's `mcp-config.json`.
    Copilot,
    /// Claude Code, through the `claude` CLI and its `settings.json`.
    ClaudeCode,
    /// Codex, through its `config.toml` and `hooks.json`.
    Codex,
    /// Gemini CLI, through its `settings.json` and `GEMINI.md`.
    Gemini,
    /// Antigravity's `mcp_config.json`, its MCP entry only.
    Antigravity,
    /// Claude Desktop's `claude_desktop_config.json`, its MCP entry only. macOS and Windows.
    ClaudeDesktop,
}

/// The harnesses that share the Copilot hook and instructions files, which read 1 home.
const COPILOT_SHARED: [Harness; 2] = [Harness::Vscode, Harness::Copilot];

impl Harness {
    /// Every harness, the first 4 in the order they were added.
    pub const ALL: [Self; 7] = [
        Self::Vscode,
        Self::Copilot,
        Self::ClaudeCode,
        Self::Codex,
        Self::Gemini,
        Self::Antigravity,
        Self::ClaudeDesktop,
    ];

    /// The harness called `name` on the command line.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "vscode" => Some(Self::Vscode),
            "copilot" => Some(Self::Copilot),
            "claude-code" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "gemini" => Some(Self::Gemini),
            "antigravity" => Some(Self::Antigravity),
            "claude-desktop" => Some(Self::ClaudeDesktop),
            _ => None,
        }
    }

    /// Its name on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vscode => "vscode",
            Self::Copilot => "copilot",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::ClaudeDesktop => "claude-desktop",
        }
    }

    /// The app's name in sentences.
    pub fn title(self) -> &'static str {
        match self {
            Self::Vscode => "VS Code",
            Self::Copilot => "Copilot CLI",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
            Self::ClaudeDesktop => "Claude Desktop",
        }
    }

    /// The `--agent` its entry passes, so turns are kept per harness.
    pub fn agent(self) -> &'static str {
        match self {
            Self::Vscode => "vscode-copilot",
            Self::Copilot => "copilot",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::ClaudeDesktop => "claude-desktop",
        }
    }

    /// The config file. VS Code's lives in its user profile. Copilot's in `$COPILOT_HOME`.
    /// Claude Code's is `.claude.json`, which only the `claude` CLI writes. Codex's is
    /// `config.toml`, which isn't JSON, so only [`codex`] reads it. Gemini CLI's is in
    /// `$GEMINI_CLI_HOME/.gemini`, else `~/.gemini`. Antigravity's is under `~/.gemini`.
    /// Claude Desktop's is in its app data dir, which only macOS and Windows have.
    pub(crate) fn file(self) -> Result<PathBuf, String> {
        Ok(match self {
            Self::Vscode if cfg!(target_os = "macos") => {
                user_home()?.join("Library/Application Support/Code/User/mcp.json")
            }
            // VS Code's docs put the user `settings.json` in `%APPDATA%\Code\User`.
            Self::Vscode if cfg!(windows) => absolute_env("APPDATA")
                .ok_or("APPDATA must be set to an absolute path")?
                .join(r"Code\User\mcp.json"),
            Self::Vscode => match absolute_env("XDG_CONFIG_HOME") {
                Some(config) => config,
                None => user_home()?.join(".config"),
            }
            .join("Code/User/mcp.json"),
            Self::Copilot => copilot_home()?.join("mcp-config.json"),
            Self::ClaudeCode => claude::state_file()?,
            Self::Codex => codex::config_file()?,
            Self::Gemini => gemini::settings_file()?,
            Self::Antigravity => plain::gemini_dir()?.join("antigravity").join("mcp_config.json"),
            Self::ClaudeDesktop => plain::desktop_dir()?.join("claude_desktop_config.json"),
        })
    }

    /// The object in the file that holds the servers.
    pub(crate) fn section(self) -> &'static str {
        match self {
            Self::Vscode => "servers",
            Self::Copilot
            | Self::ClaudeCode
            | Self::Gemini
            | Self::Antigravity
            | Self::ClaudeDesktop => "mcpServers",
            Self::Codex => "mcp_servers",
        }
    }

    /// The entry that runs `command`, passing `home` as `ATTICDB_HOME` when set.
    fn entry(self, command: &str, home: Option<&str>) -> Value {
        let entry = match self {
            // No `--session`: the adapter names the session after the repo of the client's
            // first root, else of its working folder, as the session-start hook reads it.
            // VS Code reports roots. It doesn't resolve variables in the user `mcp.json`.
            Self::Vscode => json!({"type": "stdio", "command": command,
                "args": ["mcp", "--agent", self.agent()]}),
            Self::Copilot => json!({"type": "local", "command": command,
                "args": ["mcp", "--agent", self.agent()], "tools": ["*"]}),
            Self::ClaudeCode | Self::Codex => json!({"type": "stdio", "command": command,
                "args": ["mcp", "--agent", self.agent()]}),
            // `trust` skips Gemini CLI's confirmation before each tool call.
            Self::Gemini => json!({"command": command,
                "args": ["mcp", "--agent", self.agent()], "trust": true}),
            Self::Antigravity | Self::ClaudeDesktop => json!({"command": command,
                "args": ["mcp", "--agent", self.agent()]}),
        };
        with_home(entry, home)
    }
}

/// `object` with `"env": {"ATTICDB_HOME": home}` added when `home` is set.
fn with_home(mut object: Value, home: Option<&str>) -> Value {
    if let Some(home) = home {
        object["env"] = json!({ HOME_ENV: home });
    }
    object
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

/// `$COPILOT_HOME` when set and absolute, else `~/.copilot`.
pub(crate) fn copilot_home() -> Result<PathBuf, String> {
    absolute_env("COPILOT_HOME").map_or_else(|| Ok(user_home()?.join(".copilot")), Ok)
}

/// The absolute path of the running binary, which must be UTF-8 to fit in JSON.
pub(crate) fn binary() -> Result<String, String> {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize);
    let exe = exe.map_err(|error| format!("cannot find the atticdb binary: {error}"))?;
    // `canonicalize` gives `\\?\C:\...`, which other apps may not read.
    #[cfg(windows)]
    let exe = match exe.into_os_string().into_string() {
        Ok(text) => PathBuf::from(crate::platform::plain_path(text)),
        Err(_) => return Err("the atticdb binary's path must be UTF-8".into()),
    };
    exe.into_os_string().into_string().map_err(|_| "the atticdb binary's path must be UTF-8".into())
}

/// Whether `harness` looks installed: its config exists, or its program is on `PATH`. For
/// VS Code, that is the profile dir that holds `mcp.json`. Claude Code and Codex: their config
/// dir. Copilot CLI: its `mcp-config.json` or `config.json`, not the dir, as connecting VS
/// Code writes the shared hook and instructions there. Gemini CLI: its config dir, by
/// [`gemini::config_dir`]. Antigravity: `~/.gemini/antigravity`. Claude Desktop: its app data
/// dir, never on Linux. Nothing runs.
pub fn detect(harness: Harness) -> bool {
    let copilot = |name: &str| copilot_home().ok().map(|dir| dir.join(name));
    let (paths, program) = match harness {
        Harness::Vscode => {
            (vec![harness.file().ok().and_then(|f| Some(f.parent()?.into()))], Some("code"))
        }
        Harness::Copilot => {
            (vec![copilot("mcp-config.json"), copilot("config.json")], Some("copilot"))
        }
        Harness::ClaudeCode => (vec![claude::config_dir().ok()], Some("claude")),
        Harness::Codex => (vec![codex::codex_home().ok()], Some("codex")),
        Harness::Antigravity => {
            (vec![plain::gemini_dir().ok().map(|dir| dir.join("antigravity"))], None)
        }
        Harness::Gemini => (vec![gemini::config_dir().ok()], Some("gemini")),
        Harness::ClaudeDesktop => (vec![plain::desktop_dir().ok()], None),
    };
    paths.into_iter().flatten().any(|path: PathBuf| path.exists())
        || program.and_then(claude::find_program).is_some()
}

/// Connects `harness`, or in [`Mode::Plan`] says what that would do. Prints nothing.
///
/// VS Code and Copilot CLI: adds AtticDB's entry to the harness's config and writes the hook
/// and the instructions file, after creating the home. An entry with connection flags is
/// replaced only with `force`. Under `ATTICDB_HOME`, the entry and the hook pass it on. The
/// other harness must pass the same home, as both share the hook. Else nothing changes,
/// unless `force`. The flags are checked first, so `force` is never advised past a check
/// that was not shown. `hook` and `notes` don't apply.
///
/// VS Code 1.144 and later also read Copilot CLI's config, so an entry in both files lists 2
/// servers. So while Copilot CLI has an entry, VS Code's `mcp.json` gets none, and an old one
/// there is removed by [`drop_vscode_entry`]. Connecting either one does that, so the end
/// state doesn't depend on the order. VS Code still gets the hook and instructions. Older
/// builds don't read Copilot CLI's config, so `connect vscode --force` writes `mcp.json`
/// anyway.
///
/// Claude Code: adds its MCP entry, and unless `hook` is false its SessionStart and
/// PostCompact hooks, and its allow rules for `notes`. An entry that differs is replaced only
/// with `force`.
///
/// Codex: adds its MCP entry and tool approvals for `notes`, as a block at the end of its
/// `config.toml`, unless `hook` is false its SessionStart hook, and its guide, a block in the
/// `AGENTS.md` Codex reads. An `atticdb` server that AtticDB didn't write is never edited, so
/// `force` doesn't apply. On Windows, Codex gets only its MCP entry.
///
/// Gemini CLI: adds its MCP entry to `settings.json`, with `trust` unless `notes` ask. Unless
/// `hook` is false, its SessionStart hook goes there too. Its guide is a block in `GEMINI.md`.
/// An entry with connection flags is replaced only with `force`. By [`gemini::connect`].
///
/// For Claude Code, Codex and Gemini CLI, a part that can't be applied is skipped, with what to
/// do by hand, and the others still apply.
///
/// Antigravity and Claude Desktop: adds their MCP entry only, by [`plain::connect`]. They read
/// it only at start, so the report says to restart them.
pub fn connect(harness: Harness, options: Options, mode: Mode) -> Report {
    let Options { force, hook, notes } = options;
    match harness {
        Harness::ClaudeCode => claude::connect(force, hook, notes, mode),
        Harness::Codex => codex::connect(hook, notes, mode),
        Harness::Vscode | Harness::Copilot => connect_shared(harness, force, mode, false),
        Harness::Gemini => gemini::connect(force, hook, notes, mode),
        Harness::Antigravity | Harness::ClaudeDesktop => plain::connect(harness, force, mode),
    }
}

/// [`connect`] for VS Code in a run that connects Copilot CLI just before, as setup does.
/// VS Code then reads AtticDB from Copilot CLI's config. So its plan, too, adds no entry to
/// `mcp.json`, though Copilot CLI's entry isn't written yet.
pub(crate) fn connect_vscode_after_copilot(options: Options, mode: Mode) -> Report {
    connect_shared(Harness::Vscode, options.force, mode, true)
}

/// [`connect`] for VS Code and Copilot CLI. With `copilot`, VS Code is connected as though
/// Copilot CLI had its entry.
fn connect_shared(harness: Harness, force: bool, mode: Mode, copilot: bool) -> Report {
    let (mut lines, mut hook_state) = (Vec::new(), None);
    let result = (|| {
        let moved = moved_home(harness)?;
        if !force {
            check_flags(harness, &crate::home::dir()?.join("data"))?;
        }
        let mut split = split_home(harness, moved.as_deref(), force)?;
        // Plan mode must not create the home, so it names the data dir without making it.
        let data = match mode {
            Mode::Apply => crate::home::ensure()?.data_dir,
            Mode::Plan => {
                crate::home::check_existing()?;
                crate::home::dir()?.join("data")
            }
        };
        let exe = binary()?;
        let guard = (!force).then_some(data.as_path());
        let entry = harness.entry(&exe, moved.as_deref());
        // `force` writes VS Code's own entry anyway, for builds older than 1.144.
        if harness == Harness::Vscode && !force && (copilot || copilot_entry()) {
            // Any flags on VS Code's entry were refused above.
            lines.push(drop_vscode_entry(None, mode)?);
        } else {
            lines.push(edit(harness, Some(entry), guard, mode)?);
        }
        if harness == Harness::Copilot && connected(Harness::Vscode) == Ok(true) {
            let dropped = drop_vscode_entry(guard, mode)?;
            // VS Code's entry on another home is gone, so VS Code now uses Copilot CLI's.
            if dropped.state == State::Removed {
                split = None;
            }
            lines.push(dropped);
        }
        let hook = set_hook(&exe, moved.as_deref(), mode)?;
        hook_state = Some(hook.state);
        lines.push(hook);
        lines.push(set_guide(mode)?);
        Ok::<_, String>(split)
    })();
    let mut report = match result {
        Ok(split) => Report { warnings: split.into_iter().collect(), ..Report::default() },
        Err(error) => Report::stopped(1, error),
    };
    report.lines = lines;
    report.hook = hook_state;
    report
}

/// Whether `name` is an executable on `PATH`, as AtticDB finds the harness programs.
pub fn on_path(name: &str) -> bool {
    program(name).is_some()
}

/// The program `name` on `PATH` that [`on_path`] finds, if any.
pub(crate) fn program(name: &str) -> Option<PathBuf> {
    claude::find_program(name)
}

/// Checks that `codex mcp get atticdb --json`, which only reads config, shows AtticDB's entry.
/// `Ok` without running anything when `codex` is not on `PATH`.
pub fn verify_codex() -> Result<(), String> {
    codex::verify()
}

/// Runs [`refuse_flags`] on `harness`'s current entry, before any file changes. `data` is
/// the home's data directory. A config that doesn't load is left for `edit` to report.
fn check_flags(harness: Harness, data: &Path) -> Result<(), String> {
    let path = harness.file()?;
    let Ok((_, root)) = load(&path) else { return Ok(()) };
    match root.get(harness.section()).and_then(|servers| servers.get("atticdb")) {
        Some(current) => refuse_flags(harness, &path, current, data),
        None => Ok(()),
    }
}

/// Checks the home `harness` would pass against the other connected harness's. Both share
/// the hook, which reads 1 home, so a mismatch splits memory. It is refused before any file
/// changes. With `force`, it comes back as a note to print.
fn split_home(harness: Harness, home: Option<&str>, force: bool) -> Result<Option<String>, String> {
    let ours = home_words(home);
    for other in COPILOT_SHARED.into_iter().filter(|other| *other != harness) {
        let name = other.name();
        // With HOME unset, the other config can't be located. It may still have been
        // connected from a shell where HOME was set, so its home is unknown.
        let file = match other.file() {
            Ok(file) => file,
            Err(why) if force => {
                return Ok(Some(format!(
                    "{name}'s config can't be located ({why}), so it may use another home than \
                     the one the shared sessionStart hook now reads, {ours}"
                )));
            }
            Err(why) => {
                return Err(format!(
                    "{name}'s config can't be located ({why}), so its atticdb entry may use \
                     another home than {ours}. Both share the sessionStart hook, which reads 1 \
                     home. Nothing changed. Set HOME, or add --force to go ahead anyway"
                ));
            }
        };
        let theirs = match entry_home(other, &file) {
            Ok(None) => continue,
            Ok(Some(theirs)) if theirs.as_deref().map(Path::new) == home.map(Path::new) => continue,
            Ok(Some(theirs)) => home_words(theirs.as_deref()),
            Err(why) => format!("an unknown home, as {why}"),
        };
        if force {
            return Ok(Some(format!(
                "{name} still uses {theirs}, but the shared sessionStart hook now reads {ours}. \
                 Connect {name} again under the same {HOME_ENV}"
            )));
        }
        return Err(format!(
            "{name}'s atticdb entry uses {theirs}, but this would use {ours}. Both share the \
             sessionStart hook, which reads 1 home. Nothing changed. Connect {name} again under \
             the same {HOME_ENV}, or disconnect it first. Or add --force to go ahead anyway"
        ));
    }
    Ok(None)
}

/// The home that `harness`'s AtticDB entry in `path` passes in its `env`, `Some(None)` for
/// the default home. `None` when it has no entry. An empty `ATTICDB_HOME` is the default home,
/// as [`crate::home::env_dir`] reads it. In a file that isn't plain JSON, an entry
/// counts as on the default home unless the file holds the text `ATTICDB_HOME`. Then its home
/// is unknown, and the error says why.
fn entry_home(harness: Harness, path: &Path) -> Result<Option<Option<String>>, String> {
    let mut bytes = Vec::new();
    let read = match crate::config::open_regular(OpenOptions::new().read(true), path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        opened => opened.and_then(|mut file| file.read_to_end(&mut bytes)),
    };
    read.map_err(|error| format!("{} can't be read: {error}", path.display()))?;
    let holds = |text: &[u8]| bytes.windows(text.len()).any(|window| window == text);
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(root) => Ok(root
            .get(harness.section())
            .and_then(|servers| servers.get("atticdb"))
            .map(|entry| entry["env"][HOME_ENV].as_str().filter(|home| !home.is_empty()))
            .map(|home| home.map(str::to_owned))),
        Err(_) if !holds(b"\"atticdb\"") => Ok(None),
        Err(_) if !holds(HOME_ENV.as_bytes()) => Ok(Some(None)),
        Err(_) => Err(format!("{} is not plain JSON", path.display())),
    }
}

/// `home` in words: `ATTICDB_HOME <path>`, or the default home for `None`.
fn home_words(home: Option<&str>) -> String {
    home.map_or_else(|| "the default home".into(), |home| format!("{HOME_ENV} {home}"))
}

/// `$ATTICDB_HOME` for the entry and the hook, or `None` when it is unset. A harness started
/// from the Dock doesn't get the shell's variables, so its files must name the home.
/// Copilot expands variables in a hook's env, and Claude Code in its MCP config, so a path
/// with a `$` is refused. The message names `harness`.
fn moved_home(harness: Harness) -> Result<Option<String>, String> {
    let Some(dir) = crate::home::env_dir()? else { return Ok(None) };
    let dir = dir.into_os_string().into_string();
    let dir = dir.map_err(|_| format!("{HOME_ENV} must be UTF-8 to go in a harness config"))?;
    // Copilot's docs don't say which syntax its env expansion takes on Windows, so `%` goes too.
    if cfg!(windows) && dir.contains('%') {
        return Err(format!(
            "{HOME_ENV} {dir} holds a %. {} may expand variables in its config, so atticdb would \
             read another home there. Nothing changed. Use a path without % or $",
            harness.name()
        ));
    }
    if dir.contains('$') {
        return Err(format!(
            "{HOME_ENV} {dir} holds a $. {} may expand variables in its config, so atticdb would \
             read another home there. Nothing changed. Use a path without $",
            harness.name()
        ));
    }
    Ok(Some(dir))
}

/// Removes AtticDB's entry from `harness`'s config, and the instructions file and the hook
/// once neither VS Code nor Copilot CLI has one. Both removals are tried, and every error is
/// reported. VS Code also reads Copilot CLI's entry, so while that is left, disconnecting VS
/// Code says so and keeps both shared files. Claude Code's, Codex's and Gemini CLI's parts go
/// by [`claude`]'s, [`codex`]'s and [`gemini`]'s rules, and Antigravity's and Claude Desktop's
/// by [`plain`]'s. Returns the exit code.
pub fn disconnect(harness: Harness) -> u8 {
    match harness {
        Harness::ClaudeCode => return claude::disconnect(),
        Harness::Codex => return codex::disconnect(),
        Harness::Gemini => return gemini::disconnect(),
        Harness::Antigravity | Harness::ClaudeDesktop => return plain::disconnect(harness),
        Harness::Vscode | Harness::Copilot => {}
    }
    finish(edit(harness, None, None, Mode::Apply).and_then(|line| {
        if harness == Harness::Vscode && copilot_entry() {
            let copilot = Harness::Copilot.file()?;
            if line.state != State::Unchanged {
                println!("{}", line.text);
            }
            println!(
                "VS Code 1.144 and later still read atticdb from {}. To remove it there, run \
                 atticdb disconnect copilot",
                copilot.display()
            );
        } else {
            println!("{}", line.text);
        }
        let mut errors = Vec::new();
        for removed in [remove_guide(), remove_hook()] {
            match removed {
                Ok(message) if !message.is_empty() => println!("{message}"),
                Ok(_) => {}
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(String::new())
        } else {
            Err(errors.join("\natticdb: "))
        }
    }))
}

fn finish(result: Result<String, String>) -> u8 {
    match result {
        Ok(message) => {
            if !message.is_empty() {
                println!("{message}");
            }
            0
        }
        Err(error) => {
            eprintln!("atticdb: {error}");
            1
        }
    }
}

/// Sets AtticDB's entry to `wanted`, or removes it for `None`. With `guard`, the home's data
/// directory, an entry with connection flags is refused. Returns the line to print. In Plan
/// mode, it writes nothing.
fn edit(
    harness: Harness,
    wanted: Option<Value>,
    guard: Option<&Path>,
    mode: Mode,
) -> Result<Line, String> {
    let (name, path) = (harness.name(), harness.file()?);
    let section = harness.section();
    let shown = path.display();
    let by_hand = match &wanted {
        Some(entry) => format!("add \"atticdb\": {entry} to \"{section}\" by hand"),
        None => format!("remove \"atticdb\" from \"{section}\" by hand"),
    };
    let unusable = |why: String| format!("{shown}: {why}. Nothing changed, {by_hand}");
    let (old, mut root) = load(&path).map_err(unusable)?;
    let servers = match root.entry(section).or_insert_with(|| json!({})) {
        Value::Object(servers) => servers,
        _ => return Err(unusable(format!("\"{section}\" is not an object"))),
    };
    let current = servers.get("atticdb").cloned();
    let (verb, state) = match (&wanted, &current) {
        (Some(entry), Some(current)) if entry == current => {
            return Ok(match mode {
                Mode::Plan => planned(&path, "MCP entry", State::Unchanged),
                Mode::Apply => Line::new(
                    State::Unchanged,
                    format!("atticdb is already connected to {name} ({shown})"),
                ),
            });
        }
        (None, None) => {
            return Ok(Line::new(State::Unchanged, format!("atticdb is not connected to {name}")))
        }
        (Some(entry), current) => {
            if let (Some(current), Some(data)) = (current, guard) {
                refuse_flags(harness, &path, current, data)?;
            }
            servers.insert("atticdb".into(), entry.clone());
            if current.is_some() {
                ("updated atticdb in", State::Updated)
            } else {
                ("connected atticdb to", State::Added)
            }
        }
        (None, Some(_)) => {
            servers.remove("atticdb");
            ("disconnected atticdb from", State::Removed)
        }
    };
    if mode == Mode::Plan {
        return Ok(planned(&path, "MCP entry", add_or_update(current.is_some())));
    }
    let mut text = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("{shown}: cannot encode: {error}"))?;
    text.push('\n');
    let backup = write(&path, old.as_ref(), text.as_bytes())?;
    let text = match backup {
        Some(backup) => {
            format!("{verb} {name}: {shown} (backup: {}, keys are now sorted)", backup.display())
        }
        None => format!("{verb} {name}: {shown}"),
    };
    Ok(Line::new(state, text))
}

/// Flags that point an entry at a store other than the home.
const CONNECTION: [&str; 5] = ["--url", "--data-dir", "--config", "--token-file", "--tenant"];

/// The connection flags `entry` passes, with their values, joined by `, `. Empty when none.
fn connection_flags(entry: &Value) -> String {
    let args = entry["args"].as_array().map(Vec::as_slice).unwrap_or_default();
    let args: Vec<&str> = args.iter().filter_map(Value::as_str).collect();
    let mut used = Vec::new();
    for flag in CONNECTION {
        if let Some(at) = args.iter().position(|arg| *arg == flag) {
            // `--token-file`'s value is a path, not the token.
            used.push(args.get(at + 1).map_or(flag.to_owned(), |value| format!("{flag} {value}")));
        }
    }
    used.join(", ")
}

/// Fails when `current`, `harness`'s entry in `file`, passes connection flags, because its
/// memory would not follow a switch to the home, whose data directory is `data`.
fn refuse_flags(harness: Harness, file: &Path, current: &Value, data: &Path) -> Result<(), String> {
    let used = connection_flags(current);
    if used.is_empty() {
        return Ok(());
    }
    let (file, data) = (file.display(), data.display());
    let name = harness.name();
    let apps = match harness {
        Harness::Vscode | Harness::Copilot => "VS Code and Copilot CLI",
        other => other.title(),
    };
    Err(format!(
        "{file}: the atticdb entry for {name} uses {used}. {file} was not changed.\n\
         Its memory stays in that store and would not follow a switch to the home.\n\
         To keep it, quit {apps}, then stop that store's server.\n\
         Move the files in its data directory into {data}.\n\
         Do that only while {data} is empty, and only if the old config's tenant is \"me\".\n\
         Or keep using the old entry.\n\
         Then, or to switch without it: atticdb connect {name} --force"
    ))
}

/// A file's bytes and mode.
type Saved = (Vec<u8>, u32);

/// The file's bytes and mode, and its JSON object. A missing file is an empty object.
fn load(path: &Path) -> Result<(Option<Saved>, Map<String, Value>), String> {
    let old = read(path)?;
    let root = match &old {
        Some((bytes, _)) => match serde_json::from_slice(bytes) {
            Ok(Value::Object(root)) => root,
            Ok(_) => return Err("not a JSON object".into()),
            Err(error) => return Err(format!("not plain JSON ({error})")),
        },
        None => Map::new(),
    };
    Ok((old, root))
}

/// The hook file, `hooks/atticdb.json` in `$COPILOT_HOME` or `~/.copilot`. VS Code's Copilot
/// chat and Copilot CLI both read it.
fn hook_file() -> Result<PathBuf, String> {
    Ok(copilot_home()?.join("hooks/atticdb.json"))
}

/// The hook file's text: a `sessionStart` hook that runs `exe hook session-start`, passing
/// `home` as `ATTICDB_HOME` when set.
#[cfg(not(windows))]
fn hook_text(exe: &str, home: Option<&str>) -> Result<String, String> {
    let bash = format!("'{}' hook session-start", exe.replace('\'', r"'\''"));
    let command = with_home(json!({"type": "command", "bash": bash, "timeoutSec": 5}), home);
    let hook = json!({"version": 1, "hooks": {"sessionStart": [command]}});
    let text = serde_json::to_string_pretty(&hook);
    Ok(text.map_err(|error| format!("cannot encode the hook: {error}"))? + "\n")
}

/// The hook file's text on Windows. Copilot runs the `powershell` command there, and so does
/// VS Code. `env` passes `home` as `ATTICDB_HOME` when set.
#[cfg(windows)]
fn hook_text(exe: &str, home: Option<&str>) -> Result<String, String> {
    let powershell = format!("& {} hook session-start", ps_quote(exe));
    let command =
        with_home(json!({"type": "command", "powershell": powershell, "timeoutSec": 5}), home);
    let hook = json!({"version": 1, "hooks": {"sessionStart": [command]}});
    let text = serde_json::to_string_pretty(&hook);
    Ok(text.map_err(|error| format!("cannot encode the hook: {error}"))? + "\n")
}

/// The characters PowerShell reads as a single quote.
#[cfg(any(windows, test))]
const PS_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'];

/// `word` in single quotes for PowerShell, where nothing expands: not `$`, `&` or a space.
/// Each quote character inside is written twice, which PowerShell reads as 1.
#[cfg(any(windows, test))]
pub(crate) fn ps_quote(word: &str) -> String {
    let mut quoted = String::from("'");
    for c in word.chars() {
        if PS_QUOTES.contains(&c) {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// Splits a word [`ps_quote`] wrote off the start of `text`. Returns the word and the rest.
#[cfg(any(windows, test))]
pub(crate) fn ps_unquote(text: &str) -> Option<(String, &str)> {
    let rest = text.strip_prefix('\'')?;
    let mut word = String::new();
    let mut chars = rest.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if !PS_QUOTES.contains(&c) {
            word.push(c);
            continue;
        }
        match chars.peek() {
            Some(&(_, next)) if next == c => {
                word.push(c);
                chars.next();
            }
            _ => return Some((word, &rest[at + c.len_utf8()..])),
        }
    }
    None
}

/// The binary path in `command`, a hook entry, when [`hook_text`] could have written its line.
#[cfg(not(windows))]
fn line_exe(command: &Value) -> Option<String> {
    let bash = command["bash"].as_str().unwrap_or_default();
    let quoted = bash.strip_suffix(" hook session-start").and_then(|q| q.strip_prefix('\''));
    quoted.and_then(|q| q.strip_suffix('\'')).map(|q| q.replace(r"'\''", "'"))
}

/// See the other system's version. On Windows the command is the `powershell` one.
#[cfg(windows)]
fn line_exe(command: &Value) -> Option<String> {
    let line = command["powershell"].as_str().unwrap_or_default();
    let (exe, rest) = ps_unquote(line.strip_prefix("& ")?)?;
    (rest == " hook session-start").then_some(exe)
}

/// Writes the hook file for `exe` and `home`. Returns the line to print. In Plan mode, it
/// writes nothing.
fn set_hook(exe: &str, home: Option<&str>, mode: Mode) -> Result<Line, String> {
    let (path, text) = (hook_file()?, hook_text(exe, home)?);
    let shown = path.display();
    let old = read(&path).map_err(|why| format!("{shown}: {why}. The hook was not written"))?;
    let part = "sessionStart hook";
    if old.as_ref().is_some_and(|(bytes, _)| *bytes == text.as_bytes()) {
        return Ok(match mode {
            Mode::Plan => planned(&path, part, State::Unchanged),
            Mode::Apply => Line::new(
                State::Unchanged,
                format!("the sessionStart hook is already in place ({shown})"),
            ),
        });
    }
    if mode == Mode::Plan {
        return Ok(planned(&path, part, add_or_update(old.is_some())));
    }
    Ok(match write(&path, old.as_ref(), text.as_bytes())? {
        Some(backup) => Line::new(
            State::Updated,
            format!("updated the sessionStart hook: {shown} (backup: {})", backup.display()),
        ),
        None => Line::new(State::Added, format!("added the sessionStart hook: {shown}")),
    })
}

/// Whether `bytes` are exactly the hook AtticDB writes for some absolute binary path, with
/// or without an `ATTICDB_HOME`, so a hook from a moved or other AtticDB binary counts too.
fn ours(bytes: &[u8]) -> bool {
    hook_exe(bytes).is_some()
}

/// The binary path in `bytes` when they are exactly the hook AtticDB writes, by [`ours`].
fn hook_exe(bytes: &[u8]) -> Option<String> {
    let hook: Value = serde_json::from_slice(bytes).unwrap_or_default();
    let command = &hook["hooks"]["sessionStart"][0];
    let exe = line_exe(command).filter(|exe| Path::new(exe).is_absolute())?;
    // Writing the hook again for that path and home must give the same bytes.
    let home = command["env"][HOME_ENV].as_str();
    let same = hook_text(&exe, home).is_ok_and(|text| text.as_bytes() == bytes);
    same.then_some(exe)
}

/// The AtticDB binaries that `harness`'s MCP entry and hook run: each file, with the absolute
/// path it names. A missing or unreadable file names none. It only reads.
pub fn binaries(harness: Harness) -> Vec<(PathBuf, String)> {
    match harness {
        Harness::ClaudeCode => claude::binaries(),
        Harness::Codex => codex::binaries(),
        Harness::Vscode | Harness::Copilot => {
            let mut found: Vec<_> = entry_binary(harness).into_iter().collect();
            if let Ok(path) = hook_file() {
                let exe = read(&path).ok().flatten().and_then(|(bytes, _)| hook_exe(&bytes));
                found.extend(exe.map(|exe| (path, exe)));
            }
            found
        }
        Harness::Gemini => gemini::binaries(),
        Harness::Antigravity | Harness::ClaudeDesktop => {
            entry_binary(harness).into_iter().collect()
        }
    }
}

/// The binary that `harness`'s MCP entry runs, with its config file, when the config is strict
/// JSON and the entry names an absolute path.
fn entry_binary(harness: Harness) -> Option<(PathBuf, String)> {
    let path = harness.file().ok()?;
    let (_, root) = load(&path).ok()?;
    let exe = entry_exe(root.get(harness.section())?.get("atticdb")?)?;
    Some((path, exe))
}

/// The program an MCP entry runs, when it is an absolute path.
fn entry_exe(entry: &Value) -> Option<String> {
    let command = entry["command"].as_str().filter(|command| Path::new(command).is_absolute());
    command.map(str::to_owned)
}

/// Whether `harness`'s config has an AtticDB entry. A config that is not plain JSON, such as
/// one with comments, counts when it holds `"atticdb"`.
fn connected(harness: Harness) -> Result<bool, String> {
    let Some((bytes, _)) = read(&harness.file()?)? else { return Ok(false) };
    Ok(has_entry(harness, &bytes))
}

/// Whether `bytes`, `harness`'s config, have an AtticDB entry, as [`connected`] reads them.
fn has_entry(harness: Harness, bytes: &[u8]) -> bool {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(root) => root.get(harness.section()).is_some_and(|s| s.get("atticdb").is_some()),
        Err(_) => find(bytes, b"\"atticdb\""),
    }
}

/// Whether Copilot CLI's config has an AtticDB entry, which VS Code 1.144 and later read too.
/// A config that can't be read counts as none, so VS Code keeps its own entry.
fn copilot_entry() -> bool {
    connected(Harness::Copilot).unwrap_or(false)
}

/// What VS Code builds that don't read Copilot CLI's config do to get AtticDB.
const OLDER_VSCODE: &str = "Older VS Code: update it, or run atticdb connect vscode --force";

/// Removes AtticDB's entry from VS Code's `mcp.json`, as VS Code 1.144 and later also read
/// Copilot CLI's config, and 2 entries list 2 servers. [`jsonedit`] makes the edit, so only
/// the entry's bytes change. With `guard`, an entry with connection flags is kept, as it uses
/// another store. A file that isn't strict JSON is skipped, with what to do by hand. As only
/// its raw text was searched, its line says it may hold an entry. Returns the line to print,
/// [`State::Unchanged`] when there is no entry. `Err` only when VS Code's config can't be
/// located. In Plan mode, it writes nothing.
fn drop_vscode_entry(guard: Option<&Path>, mode: Mode) -> Result<Line, String> {
    let path = Harness::Vscode.file()?;
    let shown = path.display();
    let copilot = Harness::Copilot.file();
    let copilot =
        copilot.map_or_else(|_| "Copilot CLI's config".into(), |c| c.display().to_string());
    let reads = format!("VS Code 1.144 and later read it from {copilot}");
    let skipped = |why: &str| {
        Line::new(
            State::Skipped,
            format!(
                "{shown}: {why}. Nothing changed. Remove \"atticdb\" from \"servers\" by hand. \
                 {reads}"
            ),
        )
    };
    let none = || {
        let text = format!("{shown}: no atticdb entry needed. {reads}. {OLDER_VSCODE}");
        Line::new(State::Unchanged, text)
    };
    let old = match read(&path) {
        Ok(Some(old)) if has_entry(Harness::Vscode, &old.0) => old,
        Ok(_) => return Ok(none()),
        Err(why) => return Ok(skipped(&why)),
    };
    let Ok(text) = std::str::from_utf8(&old.0) else { return Ok(skipped("not UTF-8")) };
    let (edited, had) = match jsonedit::remove_key(text, &["servers"], "atticdb") {
        Ok(done) => done,
        // serde_json can't read it, so only its raw text mentions atticdb, maybe in a comment.
        Err(error) if serde_json::from_str::<Value>(text).is_err() => {
            return Ok(Line::new(
                State::Skipped,
                format!(
                    "{shown} may hold an atticdb entry, but it isn't strict JSON: {error}. \
                     Nothing changed. If it holds one, remove it from \"servers\" by hand. \
                     {reads}"
                ),
            ));
        }
        Err(error) => return Ok(skipped(&error.to_string())),
    };
    if !had {
        return Ok(none());
    }
    if guard.is_some() {
        let root: Value = serde_json::from_str(text).unwrap_or_default();
        let used = connection_flags(&root["servers"]["atticdb"]);
        if !used.is_empty() {
            return Ok(Line::new(
                State::Kept,
                format!(
                    "{shown}: kept the atticdb entry, as it uses {used}. VS Code 1.144 and \
                     later may list atticdb twice. To remove it, run atticdb connect copilot \
                     --force"
                ),
            ));
        }
    }
    if mode == Mode::Plan {
        let text = format!("{shown}: remove the MCP entry. {reads}");
        return Ok(Line::new(State::Removed, text));
    }
    let backup = match write(&path, Some(&old), edited.as_bytes()) {
        Ok(backup) => backup.map(|backup| format!(" (backup: {})", backup.display())),
        Err(why) => return Ok(Line::new(State::Failed, why)),
    };
    Ok(Line::new(
        State::Removed,
        format!(
            "removed the duplicate atticdb entry from {shown}{}. {reads}. {OLDER_VSCODE}",
            backup.unwrap_or_default()
        ),
    ))
}

/// Why a shared Copilot file stays: VS Code or Copilot CLI still has an AtticDB entry, or its
/// config cannot be read and so may still have one. Claude Code doesn't read these files.
fn still_used() -> Option<String> {
    for harness in COPILOT_SHARED {
        let name = harness.name();
        match connected(harness) {
            Ok(true) => return Some(format!("atticdb is still connected to {name}")),
            Ok(false) => {}
            Err(error) => return Some(format!("cannot read {name}'s config: {error}")),
        }
    }
    None
}

/// Deletes `path` and syncs its directory.
fn delete(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path)
        .map_err(|error| format!("{}: cannot delete: {error}", path.display()))?;
    let parent = path.parent().unwrap_or(Path::new("/"));
    StdIo::new()
        .sync_dir(parent)
        .map_err(|error| format!("{}: cannot sync: {error}", parent.display()))
}

/// Deletes the hook file once no harness has an AtticDB entry, if AtticDB wrote it. Returns
/// the message to print, empty when there is no hook file.
fn remove_hook() -> Result<String, String> {
    let path = hook_file()?;
    let shown = path.display();
    let bytes = match read(&path) {
        Ok(Some((bytes, _))) => bytes,
        Ok(None) => return Ok(String::new()),
        Err(why) => return Ok(format!("left {shown} alone: {why}")),
    };
    if let Some(why) = still_used() {
        return Ok(format!("kept the sessionStart hook ({shown}): {why}"));
    }
    if !ours(&bytes) {
        let why = "it differs from the hook atticdb writes. Delete it by hand to stop the block";
        return Ok(format!("left {shown} alone: {why}"));
    }
    delete(&path)?;
    Ok(format!("removed the sessionStart hook: {shown}"))
}

/// The line that marks the instructions file as AtticDB's.
const MARKER: &str = "<!-- written by atticdb connect -->";

/// The instructions folder, `instructions` in `$COPILOT_HOME` or `~/.copilot`. VS Code's
/// Copilot chat and Copilot CLI both read it.
fn guide_dir() -> Result<PathBuf, String> {
    Ok(copilot_home()?.join("instructions"))
}

/// The name of AtticDB's instructions file in [`guide_dir`].
const GUIDE: &str = "atticdb.instructions.md";

/// The instructions file's text: frontmatter, [`MARKER`], then the MCP server's instructions.
fn guide_text() -> String {
    format!(
        "---\napplyTo: \"**\"\ndescription: \"How to use AtticDB, this user's long-term memory\"\n\
         ---\n{MARKER}\n\n{}\n",
        crate::mcp::INSTRUCTIONS
    )
}

/// The most of another instructions file read to look for `atticdb`.
const GUIDE_READ: u64 = 1 << 20;

/// Another `*.instructions.md` file in `dir` that mentions AtticDB, in any case, in its first
/// [`GUIDE_READ`] bytes. A backup, such as `*.instructions.md.bak-1`, doesn't count. Only
/// regular files are read, so a FIFO can't block.
fn other_guide(dir: &Path) -> Option<PathBuf> {
    let mut names: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name()).collect();
    names.sort();
    names.into_iter().map(|name| dir.join(name)).find(|path| {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        let bytes = || {
            let mut bytes = Vec::new();
            let file = crate::config::open_regular(OpenOptions::new().read(true), path);
            let _ = file.and_then(|file| file.take(GUIDE_READ).read_to_end(&mut bytes));
            bytes.to_ascii_lowercase()
        };
        name.ends_with(".instructions.md") && name != GUIDE && find(&bytes(), b"atticdb")
    })
}

/// Whether `needle` appears in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// Whether `bytes` hold the [`MARKER`] line, so AtticDB wrote them.
fn marked(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).lines().any(|line| line.trim_end() == MARKER)
}

/// Writes the instructions file, unless another file there mentions AtticDB, or a file of the
/// same name has no [`MARKER`] line, or can't be read, such as a symlink. Those are the user's
/// own, so they are kept. Returns the line to print. In Plan mode, it writes nothing.
fn set_guide(mode: Mode) -> Result<Line, String> {
    // The user's file is kept: Plan mode says so, and Apply says it skipped AtticDB's.
    let kept = |plan: String, apply: String| {
        Line::new(State::Kept, if mode == Mode::Plan { plan } else { apply })
    };
    let dir = guide_dir()?;
    if let Some(other) = other_guide(&dir) {
        let other = other.display();
        return Ok(kept(
            format!("{other}: keep the user's instructions, which mention atticdb"),
            format!("skipped the atticdb instructions: {other} already mentions atticdb"),
        ));
    }
    let (path, text) = (dir.join(GUIDE), guide_text());
    let shown = path.display();
    let old = match read(&path) {
        Ok(old) => old,
        Err(why) => {
            return Ok(kept(
                format!("{shown}: keep the user's file, which atticdb doesn't edit: {why}"),
                format!("left {shown} alone: {why}"),
            ))
        }
    };
    let part = "atticdb instructions";
    if old.as_ref().is_some_and(|(bytes, _)| *bytes == text.as_bytes()) {
        return Ok(match mode {
            Mode::Plan => planned(&path, part, State::Unchanged),
            Mode::Apply => Line::new(
                State::Unchanged,
                format!("the atticdb instructions are already in place ({shown})"),
            ),
        });
    }
    if old.as_ref().is_some_and(|(bytes, _)| !marked(bytes)) {
        return Ok(kept(
            format!("{shown}: keep the user's instructions, which have no line {MARKER}"),
            format!("skipped the atticdb instructions: {shown} has no line {MARKER}"),
        ));
    }
    if mode == Mode::Plan {
        return Ok(planned(&path, part, add_or_update(old.is_some())));
    }
    Ok(match write(&path, old.as_ref(), text.as_bytes())? {
        Some(backup) => Line::new(
            State::Updated,
            format!("updated the atticdb instructions: {shown} (backup: {})", backup.display()),
        ),
        None => Line::new(State::Added, format!("added the atticdb instructions: {shown}")),
    })
}

/// Deletes the instructions file once no harness has an AtticDB entry, if it is exactly what
/// AtticDB writes now. An edited one stays. Returns the message to print, empty when there is
/// no file.
fn remove_guide() -> Result<String, String> {
    let path = guide_dir()?.join(GUIDE);
    let shown = path.display();
    let bytes = match read(&path) {
        Ok(Some((bytes, _))) => bytes,
        Ok(None) => return Ok(String::new()),
        Err(why) => return Ok(format!("left {shown} alone: {why}")),
    };
    if let Some(why) = still_used() {
        return Ok(format!("kept the atticdb instructions ({shown}): {why}"));
    }
    if !marked(&bytes) {
        return Ok(format!("left {shown} alone: it has no line {MARKER}. Delete it by hand"));
    }
    if bytes != guide_text().as_bytes() {
        return Ok(format!(
            "left {shown} alone: it differs from the instructions atticdb writes, so it may hold \
             your edits. Delete it by hand"
        ));
    }
    delete(&path)?;
    Ok(format!("removed the atticdb instructions: {shown}"))
}

/// The file's bytes and mode, or `None` when it is missing. A symlink is refused, because
/// the rename would replace it with a regular file.
fn read(path: &Path) -> Result<Option<Saved>, String> {
    read_up_to(path, u64::MAX)
}

/// [`read`], refusing a file over `max` bytes.
fn read_up_to(path: &Path, max: u64) -> Result<Option<Saved>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(meta) if meta.is_symlink() => return Err("is a symlink".into()),
        Ok(_) => {}
    }
    let file = crate::config::open_regular(OpenOptions::new().read(true), path)
        .map_err(|error| format!("cannot read: {error}"))?;
    let mode = file.metadata().map_err(|error| format!("cannot read: {error}"))?.permissions();
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read: {error}"))?;
    if bytes.len() as u64 > max {
        return Err(format!("is larger than {max} bytes"));
    }
    Ok(Some((bytes, mode.mode() & 0o777)))
}

/// Backs up `old`, then replaces `path` with `text` through a synced temp file and a rename.
/// Creates missing parent directories. Syncs every directory entry it makes. Returns the
/// backup's path.
fn write(path: &Path, old: Option<&Saved>, text: &[u8]) -> Result<Option<PathBuf>, String> {
    let shown = path.display();
    let parent = path.parent().ok_or_else(|| format!("{shown}: has no parent directory"))?;
    let existing = parent.ancestors().find(|dir| dir.exists()).unwrap_or(Path::new("/"));
    // New directories are private, as Copilot and XDG make them. Existing ones keep their mode.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|error| format!("{}: cannot create: {error}", parent.display()))?;
    let io = StdIo::new();
    let sync = |dir: &Path| {
        io.sync_dir(dir).map_err(|error| format!("{}: cannot sync: {error}", dir.display()))
    };
    let backup = match old {
        Some((bytes, mode)) => {
            let backup = save(path, bytes, *mode)?;
            sync(parent)?;
            Some(backup)
        }
        None => None,
    };
    let file_name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let hex = crate::random_hex(8).map_err(crate::platform::no_random)?;
    let tmp = parent.join(format!(".{file_name}.atticdb-{hex}"));
    let written = create(&tmp, text, old.map(|(_, mode)| *mode))
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|error| format!("{shown}: cannot write: {error}"));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    // Deepest first, so each new directory's entry is durable once its parent is synced.
    for dir in parent.ancestors() {
        sync(dir)?;
        if dir == existing {
            break;
        }
    }
    Ok(backup)
}

/// Copies `bytes` to a new `<path>.atticdb-backup-<unix seconds>`, with a `-<n>` suffix when
/// that name is taken, and syncs it.
fn save(path: &Path, bytes: &[u8], mode: u32) -> Result<PathBuf, String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    // Built from the path's bytes, so a non-UTF-8 directory name stays intact.
    let mut base = path.as_os_str().to_owned();
    base.push(format!(".atticdb-backup-{seconds}"));
    for n in 0..100 {
        let mut backup = base.clone();
        if n > 0 {
            backup.push(format!("-{n}"));
        }
        let backup = PathBuf::from(backup);
        match create(&backup, bytes, Some(mode)) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                let _ = std::fs::remove_file(&backup);
                return Err(format!("{}: cannot write the backup: {error}", backup.display()));
            }
            Ok(()) => return Ok(backup),
        }
    }
    Err(format!("{}: too many backups in 1 second", Path::new(&base).display()))
}

/// Creates `path`, which must not exist, with `bytes` and `mode`, and syncs it. A new file
/// without `mode` is owner-only.
fn create(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    let mut file: File =
        OpenOptions::new().write(true).create_new(true).mode(mode.unwrap_or(0o600)).open(path)?;
    // Windows has no mode bits to copy. The new file is writable, as the old one was.
    #[cfg(unix)]
    if let Some(mode) = mode {
        file.set_permissions(Permissions::from_mode(mode))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{hook_exe, hook_text, ps_quote, ps_unquote, Harness};

    /// Every name parses back to its harness, and the 4 first apps keep their order.
    #[test]
    fn harness_names_round_trip() {
        let names = [
            "vscode",
            "copilot",
            "claude-code",
            "codex",
            "gemini",
            "antigravity",
            "claude-desktop",
        ];
        assert_eq!(Harness::ALL.map(Harness::name), names);
        for harness in Harness::ALL {
            assert_eq!(Harness::parse(harness.name()), Some(harness));
        }
        let agents: Vec<&str> = Harness::ALL[4..].iter().map(|h| h.agent()).collect();
        assert_eq!(agents, ["gemini", "antigravity", "claude-desktop"]);
        assert_eq!(Harness::parse("claude"), None);
    }

    /// Paths with spaces, quotes, `&` and `$`, which neither shell may read.
    const PATHS: [&str; 4] = [
        "/opt/atticdb/bin/atticdb",
        r"C:\Users\me\it's a & b $x\atticdb.exe",
        "C:\\Program Files\\\u{2018}q\u{2019} \u{201a}\u{201b}''\\atticdb.exe",
        "/home/me/'$(touch x)' `y`/atticdb",
    ];

    /// PowerShell reads `'`, `‘`, `’`, `‚` and `‛` as single quotes, so each is doubled.
    #[test]
    fn powershell_quoting_doubles_every_single_quote() {
        assert_eq!(ps_quote(r"C:\a b\x.exe"), r"'C:\a b\x.exe'");
        assert_eq!(ps_quote("it's \u{2019}"), "'it''s \u{2019}\u{2019}'");
        for path in PATHS {
            let quoted = ps_quote(path);
            assert_eq!(ps_unquote(&format!("{quoted} rest")), Some((path.into(), " rest")));
        }
        assert_eq!(ps_unquote("'open"), None);
        assert_eq!(ps_unquote("plain"), None);
    }

    /// The hook file names the binary so that reading it back gives the same path and home.
    #[test]
    fn the_hook_file_reads_back_its_binary() {
        for path in PATHS.iter().filter(|path| std::path::Path::new(path).is_absolute()) {
            for home in [None, Some("/h o m e"), Some(r"C:\Users\me\h o m e")] {
                let text = hook_text(path, home).unwrap();
                assert_eq!(hook_exe(text.as_bytes()).as_deref(), Some(*path), "{text}");
            }
        }
        assert_eq!(hook_exe(b"{}"), None);
    }
}
