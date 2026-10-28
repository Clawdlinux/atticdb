//! `zerodb connect` and `zerodb disconnect`: add or remove zerodb's entry in a harness's
//! user MCP config, and the Copilot `sessionStart` hook file both harnesses share. The
//! entry and the hook run the binary on the home, so they hold no token or path but the
//! binary's.

use serde_json::{json, Map, Value};
use std::fs::{File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zerodb_tape::{Io, StdIo};

/// A harness whose user MCP config zerodb can edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    /// VS Code's `mcp.json`, used by its Copilot chat.
    Vscode,
    /// Copilot CLI's `mcp-config.json`.
    Copilot,
}

impl Harness {
    /// Every harness.
    pub const ALL: [Self; 2] = [Self::Vscode, Self::Copilot];

    /// The harness called `name` on the command line.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "vscode" => Some(Self::Vscode),
            "copilot" => Some(Self::Copilot),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Vscode => "vscode",
            Self::Copilot => "copilot",
        }
    }

    /// The `--agent` its entry passes, so turns are kept per harness.
    pub fn agent(self) -> &'static str {
        match self {
            Self::Vscode => "vscode-copilot",
            Self::Copilot => "copilot",
        }
    }

    /// The config file. VS Code's lives in its user profile. Copilot's in `$COPILOT_HOME`.
    fn file(self) -> Result<PathBuf, String> {
        Ok(match self {
            Self::Vscode if cfg!(target_os = "macos") => {
                home()?.join("Library/Application Support/Code/User/mcp.json")
            }
            Self::Vscode => match absolute_env("XDG_CONFIG_HOME") {
                Some(config) => config,
                None => home()?.join(".config"),
            }
            .join("Code/User/mcp.json"),
            Self::Copilot => copilot_home()?.join("mcp-config.json"),
        })
    }

    /// The object in the file that holds the servers.
    fn section(self) -> &'static str {
        match self {
            Self::Vscode => "servers",
            Self::Copilot => "mcpServers",
        }
    }

    fn entry(self, command: &str) -> Value {
        match self {
            Self::Vscode => {
                let session = "${workspaceFolderBasename}";
                json!({"type": "stdio", "command": command,
                    "args": ["mcp", "--agent", self.agent(), "--session", session]})
            }
            Self::Copilot => json!({"type": "local", "command": command,
                "args": ["mcp", "--agent", self.agent()], "tools": ["*"]}),
        }
    }
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

fn home() -> Result<PathBuf, String> {
    absolute_env("HOME").ok_or_else(|| "HOME must be set to an absolute path".into())
}

/// `$COPILOT_HOME` when set and absolute, else `~/.copilot`.
fn copilot_home() -> Result<PathBuf, String> {
    absolute_env("COPILOT_HOME").map_or_else(|| Ok(home()?.join(".copilot")), Ok)
}

/// The absolute path of the running binary, which must be UTF-8 to fit in JSON.
fn binary() -> Result<String, String> {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize);
    let exe = exe.map_err(|error| format!("cannot find the zerodb binary: {error}"))?;
    exe.into_os_string().into_string().map_err(|_| "the zerodb binary's path must be UTF-8".into())
}

/// Adds zerodb's entry to `harness`'s config and writes the hook, after creating the home.
/// Returns the exit code.
pub fn connect(harness: Harness) -> u8 {
    finish(crate::home::ensure().and_then(|_| binary()).and_then(|exe| {
        println!("{}", edit(harness, Some(harness.entry(&exe)))?);
        set_hook(&exe)
    }))
}

/// Removes zerodb's entry from `harness`'s config, and the hook once no harness has one.
/// Returns the exit code.
pub fn disconnect(harness: Harness) -> u8 {
    finish(edit(harness, None).and_then(|message| {
        println!("{message}");
        remove_hook()
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
            eprintln!("zerodb: {error}");
            1
        }
    }
}

/// Sets zerodb's entry to `wanted`, or removes it for `None`. Returns the message to print.
fn edit(harness: Harness, wanted: Option<Value>) -> Result<String, String> {
    let (name, path) = (harness.name(), harness.file()?);
    let section = harness.section();
    let shown = path.display();
    let by_hand = match &wanted {
        Some(entry) => format!("add \"zerodb\": {entry} to \"{section}\" by hand"),
        None => format!("remove \"zerodb\" from \"{section}\" by hand"),
    };
    let unusable = |why: String| format!("{shown}: {why}. Nothing changed, {by_hand}");
    let (old, mut root) = load(&path).map_err(unusable)?;
    let servers = match root.entry(section).or_insert_with(|| json!({})) {
        Value::Object(servers) => servers,
        _ => return Err(unusable(format!("\"{section}\" is not an object"))),
    };
    let current = servers.get("zerodb").cloned();
    let verb = match (&wanted, &current) {
        (Some(entry), Some(current)) if entry == current => {
            return Ok(format!("zerodb is already connected to {name} ({shown})"));
        }
        (None, None) => return Ok(format!("zerodb is not connected to {name}")),
        (Some(entry), current) => {
            servers.insert("zerodb".into(), entry.clone());
            if current.is_some() {
                "updated zerodb in"
            } else {
                "connected zerodb to"
            }
        }
        (None, Some(_)) => {
            servers.remove("zerodb");
            "disconnected zerodb from"
        }
    };
    let mut text = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("{shown}: cannot encode: {error}"))?;
    text.push('\n');
    let backup = write(&path, old.as_ref(), text.as_bytes())?;
    Ok(match backup {
        Some(backup) => {
            format!("{verb} {name}: {shown} (backup: {}, keys are now sorted)", backup.display())
        }
        None => format!("{verb} {name}: {shown}"),
    })
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

/// The hook file, `hooks/zerodb.json` in `$COPILOT_HOME` or `~/.copilot`. VS Code's Copilot
/// chat and Copilot CLI both read it.
fn hook_file() -> Result<PathBuf, String> {
    Ok(copilot_home()?.join("hooks/zerodb.json"))
}

/// The hook file's text: a `sessionStart` hook that runs `exe hook session-start`.
fn hook_text(exe: &str) -> Result<String, String> {
    let bash = format!("'{}' hook session-start", exe.replace('\'', r"'\''"));
    let hook = json!({"version": 1, "hooks": {"sessionStart": [
        {"type": "command", "bash": bash, "timeoutSec": 5}]}});
    let text = serde_json::to_string_pretty(&hook);
    Ok(text.map_err(|error| format!("cannot encode the hook: {error}"))? + "\n")
}

/// Writes the hook file for `exe`. Returns the message to print.
fn set_hook(exe: &str) -> Result<String, String> {
    let (path, text) = (hook_file()?, hook_text(exe)?);
    let shown = path.display();
    let old = read(&path).map_err(|why| format!("{shown}: {why}. The hook was not written"))?;
    if old.as_ref().is_some_and(|(bytes, _)| *bytes == text.as_bytes()) {
        return Ok(format!("the sessionStart hook is already in place ({shown})"));
    }
    Ok(match write(&path, old.as_ref(), text.as_bytes())? {
        Some(backup) => {
            format!("updated the sessionStart hook: {shown} (backup: {})", backup.display())
        }
        None => format!("added the sessionStart hook: {shown}"),
    })
}

/// Whether `bytes` are exactly the hook zerodb writes for some absolute binary path, so a
/// hook from a moved or other zerodb binary counts too.
fn ours(bytes: &[u8]) -> bool {
    let hook: Value = serde_json::from_slice(bytes).unwrap_or_default();
    let bash = hook["hooks"]["sessionStart"][0]["bash"].as_str().unwrap_or_default();
    let quoted = bash.strip_suffix(" hook session-start").and_then(|q| q.strip_prefix('\''));
    let exe = quoted.and_then(|q| q.strip_suffix('\'')).map(|q| q.replace(r"'\''", "'"));
    let exe = exe.filter(|exe| Path::new(exe).is_absolute());
    // Writing the hook again for that path must give the same bytes.
    exe.and_then(|exe| hook_text(&exe).ok()).is_some_and(|text| text.as_bytes() == bytes)
}

/// Deletes the hook file once no harness has a zerodb entry, if zerodb wrote it. Returns
/// the message to print, empty when there is no hook file.
fn remove_hook() -> Result<String, String> {
    let path = hook_file()?;
    let shown = path.display();
    let bytes = match read(&path) {
        Ok(Some((bytes, _))) => bytes,
        Ok(None) => return Ok(String::new()),
        Err(why) => return Ok(format!("left {shown} alone: {why}")),
    };
    let kept = format!("kept the sessionStart hook ({shown})");
    for harness in Harness::ALL {
        let name = harness.name();
        match harness.file().and_then(|file| load(&file)) {
            Ok((_, root))
                if root.get(harness.section()).is_some_and(|s| s.get("zerodb").is_some()) =>
            {
                return Ok(format!("{kept}: zerodb is still connected to {name}"));
            }
            Ok(_) => {}
            // A config that cannot be read may still have the entry.
            Err(error) => return Ok(format!("{kept}: cannot read {name}'s config: {error}")),
        }
    }
    if !ours(&bytes) {
        let why = "it differs from the hook zerodb writes. Delete it by hand to stop the block";
        return Ok(format!("left {shown} alone: {why}"));
    }
    std::fs::remove_file(&path).map_err(|error| format!("{shown}: cannot delete: {error}"))?;
    let parent = path.parent().unwrap_or(Path::new("/"));
    StdIo::new()
        .sync_dir(parent)
        .map_err(|error| format!("{}: cannot sync: {error}", parent.display()))?;
    Ok(format!("removed the sessionStart hook: {shown}"))
}

/// The file's bytes and mode, or `None` when it is missing. A symlink is refused, because
/// the rename would replace it with a regular file.
fn read(path: &Path) -> Result<Option<Saved>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(meta) if meta.is_symlink() => return Err("is a symlink".into()),
        Ok(_) => {}
    }
    let mut file = crate::config::open_regular(OpenOptions::new().read(true), path)
        .map_err(|error| format!("cannot read: {error}"))?;
    let mode = file.metadata().map_err(|error| format!("cannot read: {error}"))?.permissions();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .map_err(|error| format!("cannot read: {error}"))?;
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
    let hex = crate::random_hex(8).map_err(|error| format!("cannot read /dev/urandom: {error}"))?;
    let tmp = parent.join(format!(".{file_name}.zerodb-{hex}"));
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

/// Copies `bytes` to a new `<path>.zerodb-backup-<unix seconds>`, with a `-<n>` suffix when
/// that name is taken, and syncs it.
fn save(path: &Path, bytes: &[u8], mode: u32) -> Result<PathBuf, String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    // Built from the path's bytes, so a non-UTF-8 directory name stays intact.
    let mut base = path.as_os_str().to_owned();
    base.push(format!(".zerodb-backup-{seconds}"));
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

/// Creates `path`, which must not exist, with `bytes` and `mode`, and syncs it.
fn create(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    let mut file: File =
        OpenOptions::new().write(true).create_new(true).mode(mode.unwrap_or(0o666)).open(path)?;
    if let Some(mode) = mode {
        file.set_permissions(Permissions::from_mode(mode))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}
