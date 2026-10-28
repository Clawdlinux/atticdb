//! `zerodb connect` and `zerodb disconnect`: add or remove zerodb's entry in a harness's
//! user MCP config. The entry runs `zerodb mcp` on the home, so it holds no token or path
//! but the binary's.

use serde_json::{json, Map, Value};
use std::fs::{File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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

    /// The config file. VS Code's lives in its user profile. Copilot's in `$COPILOT_HOME`.
    fn file(self) -> Result<PathBuf, String> {
        let home = absolute_env("HOME").ok_or("HOME must be set to an absolute path")?;
        Ok(match self {
            Self::Vscode if cfg!(target_os = "macos") => {
                home.join("Library/Application Support/Code/User/mcp.json")
            }
            Self::Vscode => absolute_env("XDG_CONFIG_HOME")
                .unwrap_or_else(|| home.join(".config"))
                .join("Code/User/mcp.json"),
            Self::Copilot => absolute_env("COPILOT_HOME")
                .unwrap_or_else(|| home.join(".copilot"))
                .join("mcp-config.json"),
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
                    "args": ["mcp", "--agent", "vscode-copilot", "--session", session]})
            }
            Self::Copilot => json!({"type": "local", "command": command,
                "args": ["mcp", "--agent", "copilot"], "tools": ["*"]}),
        }
    }
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

/// Adds zerodb's entry to `harness`'s config, after creating the home. Returns the exit code.
pub fn connect(harness: Harness) -> u8 {
    let entry = crate::home::ensure().and_then(|_| {
        let exe = std::env::current_exe().and_then(std::fs::canonicalize);
        let exe = exe.map_err(|error| format!("cannot find the zerodb binary: {error}"))?;
        let exe = exe.to_str().ok_or("the zerodb binary's path must be UTF-8")?;
        Ok(harness.entry(exe))
    });
    finish(entry.and_then(|entry| edit(harness, Some(entry))))
}

/// Removes zerodb's entry from `harness`'s config. Returns the exit code.
pub fn disconnect(harness: Harness) -> u8 {
    finish(edit(harness, None))
}

fn finish(result: Result<String, String>) -> u8 {
    match result {
        Ok(message) => {
            println!("{message}");
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
    let old = read(&path).map_err(unusable)?;
    let mut root = match &old {
        Some((bytes, _)) => match serde_json::from_slice(bytes) {
            Ok(Value::Object(root)) => root,
            Ok(_) => return Err(unusable("not a JSON object".into())),
            Err(error) => return Err(unusable(format!("not plain JSON ({error})"))),
        },
        None => Map::new(),
    };
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

/// The file's bytes and mode, or `None` when it is missing. A symlink is refused, because
/// the rename would replace it with a regular file.
fn read(path: &Path) -> Result<Option<(Vec<u8>, u32)>, String> {
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
fn write(
    path: &Path,
    old: Option<&(Vec<u8>, u32)>,
    text: &[u8],
) -> Result<Option<PathBuf>, String> {
    let shown = path.display();
    let parent = path.parent().ok_or_else(|| format!("{shown}: has no parent directory"))?;
    let existing = parent.ancestors().find(|dir| dir.exists()).unwrap_or(Path::new("/"));
    std::fs::create_dir_all(parent)
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
    let base = format!("{}.zerodb-backup-{seconds}", path.display());
    for n in 0..100 {
        let backup = PathBuf::from(if n == 0 { base.clone() } else { format!("{base}-{n}") });
        match create(&backup, bytes, Some(mode)) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                let _ = std::fs::remove_file(&backup);
                return Err(format!("{}: cannot write the backup: {error}", backup.display()));
            }
            Ok(()) => return Ok(backup),
        }
    }
    Err(format!("{base}: too many backups in 1 second"))
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
