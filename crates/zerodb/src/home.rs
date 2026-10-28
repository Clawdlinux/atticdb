//! The per-user home that `zerodb mcp` and the shell verbs use when given no connection
//! flags: an owner-only directory with `zerodb.json` and `data/`, created on first use.

use std::path::PathBuf;

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
        if let Err(error) = crate::config::init(&config, TENANT) {
            // A concurrent first run that created it first is fine.
            if !config.exists() {
                return Err(error);
            }
        }
    }
    let data_dir = dir.join("data");
    crate::listen::owned_dir(&data_dir, 0o077)?;
    Ok(Home { config, data_dir, tenant: TENANT.into() })
}
