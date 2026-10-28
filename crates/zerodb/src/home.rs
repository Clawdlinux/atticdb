//! The per-user home that `zerodb mcp` and the shell verbs use when given no connection
//! flags: an owner-only directory with `zerodb.json` and `data/`, created on first use.

use std::path::{Path, PathBuf};

/// The tenant the home's config names.
pub const TENANT: &str = "me";

/// A home ready for use.
#[derive(Clone, Debug)]
pub struct Home {
    /// `zerodb.json`, mode 0600.
    pub config: PathBuf,
    /// `data/`, mode 0700.
    pub data_dir: PathBuf,
    /// The tenant in `config`.
    pub tenant: String,
}

/// The home directory. On macOS `$HOME/Library/Application Support/zerodb`. On Linux
/// `$XDG_DATA_HOME/zerodb` when that is set and absolute, else `$HOME/.local/share/zerodb`.
pub fn dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
    let home = home.ok_or("HOME must be set to an absolute path")?;
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
    Ok(Home { config, data_dir, tenant: TENANT.into() })
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
