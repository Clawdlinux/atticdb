//! The per-user home that `zerodb mcp` and the shell verbs use when given no connection
//! flags: an owner-only directory with `zerodb.json` and `data/`, created on first use.

use std::path::{Path, PathBuf};
use zerodb_tape::{Io, StdIo};

/// The tenant the home's config names.
pub const TENANT: &str = "me";

/// A home ready for use.
#[derive(Clone, Debug)]
pub struct Home {
    /// The home directory, mode 0700.
    pub dir: PathBuf,
    /// `zerodb.json`, mode 0600.
    pub config: PathBuf,
    /// `data/`, mode 0700.
    pub data_dir: PathBuf,
    /// The tenant in `config`.
    pub tenant: String,
}

/// The repo name for `dir`, an absolute path: the name of the first directory from `dir` up
/// that has a `.git` entry, else of `dir`. `None` when that has no UTF-8 name. It is the
/// default session and what the session-start hook reads, so notes land where it looks.
/// Symlinks are resolved first, as the adapter's `current_dir` is. A path that cannot be
/// resolved, such as a deleted folder, is walked as given.
pub fn repo(dir: &Path) -> Option<String> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
    let root = dir.ancestors().find(|dir| dir.join(".git").symlink_metadata().is_ok());
    root.unwrap_or(&dir).file_name().and_then(|name| name.to_str()).map(str::to_owned)
}

/// `$HOME`, which must be absolute.
pub fn user_home() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
    home.ok_or_else(|| "HOME must be set to an absolute path".into())
}

/// The home directory. On macOS `$HOME/Library/Application Support/zerodb`. On Linux
/// `$XDG_DATA_HOME/zerodb` when that is set and absolute, else `$HOME/.local/share/zerodb`.
pub fn dir() -> Result<PathBuf, String> {
    let home = user_home()?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Application Support/zerodb"));
    }
    let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    Ok(xdg
        .filter(|xdg| xdg.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("zerodb"))
}

/// Creates the home if missing: the directory and `data/` with mode 0700, and
/// `zerodb.json` with tenant [`TENANT`]. Checks both directories are owner-only.
pub fn ensure() -> Result<Home, String> {
    let dir = dir()?;
    crate::listen::owned_dir(&dir, 0o077)?;
    let config = dir.join("zerodb.json");
    if !config.exists() {
        publish(&dir, &config)?;
    }
    let data_dir = dir.join("data");
    crate::listen::owned_dir(&data_dir, 0o077)?;
    // Every call syncs, covering a concurrent creator that has not synced yet.
    sync_dirs(&dir, &data_dir)?;
    Ok(Home { dir, config, data_dir, tenant: TENANT.into() })
}

/// Syncs `data/`, the home and its ancestors, deepest first, so their entries survive a
/// power loss before the first ack.
fn sync_dirs(dir: &Path, data_dir: &Path) -> Result<(), String> {
    let home = user_home()?;
    // Stop at $HOME, or at XDG_DATA_HOME's parent when the home is outside $HOME.
    let top = if dir.starts_with(&home) {
        home.as_path()
    } else {
        dir.ancestors().nth(2).unwrap_or(Path::new("/"))
    };
    let io = StdIo::new();
    for path in data_dir.ancestors() {
        io.sync_dir(path).map_err(|error| format!("{}: cannot sync: {error}", path.display()))?;
        if path == top {
            break;
        }
    }
    Ok(())
}

/// Writes a complete config to a temp file, then hard-links it to `config`, so `config`
/// never appears empty or partial. A concurrent creator winning the race is fine. A temp
/// file left by a killed run is ignored.
fn publish(dir: &Path, config: &Path) -> Result<(), String> {
    let hex = crate::random_hex(8).map_err(|error| format!("cannot read /dev/urandom: {error}"))?;
    let tmp = dir.join(format!(".zerodb.json.{hex}"));
    crate::config::init(&tmp, TENANT)?;
    let linked = match std::fs::hard_link(&tmp, config) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
            Err(format!("config {}: cannot create: {error}", config.display()))
        }
        _ => Ok(()),
    };
    let _ = std::fs::remove_file(&tmp);
    linked
}
