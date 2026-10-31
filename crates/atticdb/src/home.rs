//! The per-user home that `atticdb mcp` and the shell verbs use when given no connection
//! flags: an owner-only directory with `atticdb.json` and `data/`, created on first use.

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The tenant the home's config names.
pub const TENANT: &str = "me";
/// The home's directory name at its default path.
pub(crate) const NAME: &str = "atticdb";
/// The home's config file.
pub(crate) const CONFIG: &str = "atticdb.json";
/// The variable that moves the home. Empty counts as unset.
pub const HOME_ENV: &str = "ATTICDB_HOME";
/// Most bytes read from a `.git` or `commondir` file. A longer one names no real path.
const MAX_GIT_FILE: u64 = 8 << 10;

/// A home ready for use.
#[derive(Clone, Debug)]
pub struct Home {
    /// The home directory, mode 0700.
    pub dir: PathBuf,
    /// `atticdb.json`, mode 0600.
    pub config: PathBuf,
    /// `data/`, mode 0700.
    pub data_dir: PathBuf,
    /// The tenant in `config`.
    pub tenant: String,
}

/// The repo name for `dir`, an absolute path. The root is the first directory from `dir` up
/// with a `.git` entry, else `dir`. A worktree takes its main repo's name, read by
/// `main_repo`. Otherwise the name is the root's. `None` when there is no UTF-8 name. It is
/// the default session and what the session-start hook reads, so notes land where it looks.
/// Symlinks are resolved first, as the adapter's `current_dir` is. A path that cannot be
/// resolved, such as a deleted folder, is walked as given.
pub fn repo(dir: &Path) -> Option<String> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
    let root = dir.ancestors().find(|dir| dir.join(".git").symlink_metadata().is_ok());
    let main = root.and_then(|root| main_repo(&root.join(".git")));
    main.or_else(|| root.unwrap_or(&dir).file_name()?.to_str().map(str::to_owned))
}

/// The main repo's name when `git`, a `.git` entry, is a worktree's file `gitdir: <dir>`
/// and `<dir>/commondir` names the common git dir. A common dir whose name starts with `.`,
/// such as `.git` or a bare `.bare`, gives its parent's name. Any other gives its own name,
/// without a `.git` suffix. `None` for a `.git` directory, a file without `commondir` such
/// as a submodule's, a common dir that is not a directory, or anything unreadable.
fn main_repo(git: &Path) -> Option<String> {
    let git_dir = path_in(git, "gitdir: ")?;
    let common = path_in(&git_dir.join("commondir"), "")?;
    let common = std::fs::canonicalize(common).ok().filter(|common| common.is_dir())?;
    let name = common.file_name()?.to_str()?;
    if name.starts_with('.') {
        return common.parent()?.file_name()?.to_str().map(str::to_owned);
    }
    Some(name.strip_suffix(".git").unwrap_or(name).to_owned())
}

/// The path in the regular file `file`: the text after `prefix`, less trailing line ends. A
/// relative path counts from the file's directory. `None` when the file is missing, not a
/// regular file, over [`MAX_GIT_FILE`], or holds no such path.
fn path_in(file: &Path, prefix: &str) -> Option<PathBuf> {
    let opened = crate::config::open_regular(OpenOptions::new().read(true), file).ok()?;
    let mut text = Vec::new();
    opened.take(MAX_GIT_FILE + 1).read_to_end(&mut text).ok()?;
    if text.len() as u64 > MAX_GIT_FILE {
        return None;
    }
    let path = text.strip_prefix(prefix.as_bytes())?;
    let end = path.iter().rposition(|byte| !matches!(byte, b'\n' | b'\r'))?;
    #[cfg(unix)]
    let path = OsStr::from_bytes(&path[..=end]);
    // Git for Windows writes these paths as UTF-8.
    #[cfg(not(unix))]
    let path = std::str::from_utf8(&path[..=end]).ok()?;
    Some(file.parent()?.join(path))
}

/// `$HOME`, which must be absolute.
#[cfg(unix)]
pub fn user_home() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
    home.ok_or_else(|| "HOME must be set to an absolute path".into())
}

/// `%USERPROFILE%`, which must be absolute. Apps keep their config dirs there.
#[cfg(not(unix))]
pub fn user_home() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let home = home.filter(|home| home.is_absolute());
    home.ok_or_else(|| "USERPROFILE must be set to an absolute path".into())
}

/// `$ATTICDB_HOME` when it is set and not empty. A relative path is an error, so nothing
/// falls back to the default home.
pub fn env_dir() -> Result<Option<PathBuf>, String> {
    match std::env::var_os(HOME_ENV).filter(|dir| !dir.is_empty()).map(PathBuf::from) {
        Some(dir) if !dir.is_absolute() => Err(format!("{HOME_ENV} must be an absolute path")),
        dir => Ok(dir),
    }
}

/// The home directory, by [`Lookup::ENV`].
pub fn dir() -> Result<PathBuf, String> {
    Lookup::ENV.dir()
}

/// How to find the home. Commands read `ATTICDB_HOME` and run the legacy guards, by
/// [`Lookup::ENV`]. `atticdb migrate` passes the home it resolved and skips the guards, as old
/// names may still be set in its environment.
#[derive(Clone, Debug)]
pub struct Lookup {
    /// `None` reads `ATTICDB_HOME`. `Some` stands for its value, `Some(None)` for unset.
    given: Option<Option<PathBuf>>,
    /// Whether [`crate::legacy`]'s guards run.
    guards: bool,
}

impl Lookup {
    /// The home the environment names, with the legacy guards.
    pub const ENV: Self = Self { given: None, guards: true };

    /// The home `dir` names as `ATTICDB_HOME` would, the default home for `None`, without
    /// the legacy guards.
    #[cfg(unix)]
    pub(crate) fn given(dir: Option<PathBuf>) -> Self {
        Self { given: Some(dir), guards: false }
    }

    /// `ATTICDB_HOME` as [`env_dir`] reads it, or as given.
    pub(crate) fn env_dir(&self) -> Result<Option<PathBuf>, String> {
        match &self.given {
            Some(dir) => Ok(dir.clone()),
            None => env_dir(),
        }
    }

    /// The home directory: [`Self::env_dir`] when set, else [`default_dir`]. With the guards,
    /// it refuses by [`crate::legacy::check_home`] when data from before the rename is in the
    /// way, so nothing makes a new home beside it.
    pub fn dir(&self) -> Result<PathBuf, String> {
        if self.guards {
            crate::legacy::check_env()?;
        }
        let (dir, default) = match self.env_dir()? {
            Some(dir) => (dir, false),
            None => (default_dir(NAME)?, true),
        };
        if self.guards {
            crate::legacy::check_home(&dir, default)?;
        }
        #[cfg(windows)]
        crate::platform::check_in_profile(&dir, "home")?;
        Ok(dir)
    }

    /// Gives a child `atticdb` this home: `ATTICDB_HOME` when given, and without the old
    /// names when the guards are off, so the child finds the same home.
    pub(crate) fn pass(&self, command: &mut std::process::Command) {
        match &self.given {
            Some(Some(dir)) => drop(command.env(HOME_ENV, dir)),
            Some(None) => drop(command.env_remove(HOME_ENV)),
            None => {}
        }
        if !self.guards {
            crate::legacy::clear_env(command);
        }
    }
}

/// The home directory as [`dir`] finds it, without its checks, and whether it is the default.
#[cfg(unix)]
pub(crate) fn unchecked_dir() -> Result<(PathBuf, bool), String> {
    match env_dir()? {
        Some(dir) => Ok((dir, false)),
        None => Ok((default_dir(NAME)?, true)),
    }
}

/// The default home called `name`. On macOS `$HOME/Library/Application Support/<name>`. On
/// Linux `$XDG_DATA_HOME/<name>` when that is set and absolute, else
/// `$HOME/.local/share/<name>`.
#[cfg(unix)]
pub(crate) fn default_dir(name: &str) -> Result<PathBuf, String> {
    let home = user_home()?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Application Support").join(name));
    }
    let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    Ok(xdg.filter(|xdg| xdg.is_absolute()).unwrap_or_else(|| home.join(".local/share")).join(name))
}

/// The default home called `name`: `%LOCALAPPDATA%\<name>`, the user's local app data.
#[cfg(not(unix))]
pub(crate) fn default_dir(name: &str) -> Result<PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let local = local.filter(|local| local.is_absolute()).ok_or_else(|| {
        format!("LOCALAPPDATA must be set to an absolute path, or set {HOME_ENV}")
    })?;
    Ok(local.join(name))
}

/// Creates the home if missing, by [`Lookup::ENV`].
pub fn ensure() -> Result<Home, String> {
    Lookup::ENV.ensure()
}

/// Checks the home and its `data/`, by [`Lookup::ENV`].
pub fn check_existing() -> Result<(), String> {
    Lookup::ENV.check_existing()
}

impl Lookup {
    /// Creates the home if missing: the directory and `data/` with mode 0700, and
    /// `atticdb.json` with tenant [`TENANT`]. Checks both directories are owner-only.
    pub fn ensure(&self) -> Result<Home, String> {
        let dir = self.dir()?;
        // With $HOME unset, as ATTICDB_HOME allows, the home counts as outside it.
        let home = user_home().ok().filter(|home| dir.starts_with(home));
        // A $HOME missing now is made below, and nothing above it is known to be synced.
        let made_home = home.as_deref().is_some_and(|home| home.symlink_metadata().is_err());
        let stop = home.as_deref().filter(|_| !made_home).and_then(Path::parent);
        // The parent of the highest directory this call makes holds that directory's new entry,
        // so it must be synced even when this user can't read it.
        let data_dir = dir.join("data");
        let made = data_dir.ancestors().take_while(|path| path.symlink_metadata().is_err()).last();
        let holder = made.and_then(Path::parent).map(Path::to_owned);
        crate::listen::owned_dir(&dir, 0o077)?;
        let config = dir.join(CONFIG);
        if !config.exists() {
            publish(&dir, &config)?;
        }
        crate::listen::owned_dir(&data_dir, 0o077)?;
        // Every call syncs, covering a concurrent creator that has not synced yet.
        sync_dirs(&dir, &data_dir, stop, holder.as_deref())?;
        Ok(Home { dir, config, data_dir, tenant: TENANT.into() })
    }

    /// Checks the home and its `data/`, where they exist, as [`Self::ensure`] does: each must
    /// be a directory owned by this user, mode 0700 or stricter. A symlink counts as existing,
    /// even a dangling one, as [`Self::ensure`] refuses that too. It creates nothing, so a plan
    /// shows the refusal that [`Self::ensure`] would give.
    pub fn check_existing(&self) -> Result<(), String> {
        let home = self.dir()?;
        for dir in [home.clone(), home.join("data")] {
            match std::fs::symlink_metadata(&dir) {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                _ => crate::listen::check_owned(&dir, 0o077)?,
            }
        }
        Ok(())
    }
}

/// Syncs the existing home `dir` and its ancestors as [`ensure`] does, after it was moved or
/// a file in it was renamed.
#[cfg(unix)]
pub(crate) fn sync(dir: &Path) -> Result<(), String> {
    let home = user_home().ok().filter(|home| dir.starts_with(home));
    let data = dir.join("data");
    let deepest = if data.is_dir() { data } else { dir.to_owned() };
    sync_dirs(dir, &deepest, home.as_deref().and_then(Path::parent), None)
}

/// Syncs `data/`, the home and its ancestors, deepest first, so their entries survive a
/// power loss before the first ack. It stops after syncing `stop`, else at `/`. A home
/// inside `$HOME` stops at `$HOME`'s parent, so a `$HOME` that another call made is synced
/// too. Any other home, or a `$HOME` this call made, syncs every ancestor up to `/`, since
/// a first use may have made any of them. `holder` holds the entry of the highest directory
/// this call made, so it is never skipped.
#[cfg(unix)]
fn sync_dirs(
    dir: &Path,
    data_dir: &Path,
    stop: Option<&Path>,
    holder: Option<&Path>,
) -> Result<(), String> {
    for path in data_dir.ancestors() {
        let fail = |error: std::io::Error| format!("{}: cannot sync: {error}", path.display());
        match File::open(path) {
            Err(error) if holder == Some(path) => {
                return Err(format!(
                    "{}: cannot sync the entry for the directory made in it: {error}. Make it \
                     readable, or create the home by hand",
                    path.display()
                ));
            }
            // An ancestor above the home that this user can't open existed before, as every
            // directory AtticDB makes is owner-only. Its entries were already there. EACCES and
            // EPERM both land here.
            Err(error)
                if error.kind() == ErrorKind::PermissionDenied
                    && path != dir
                    && dir.starts_with(path) => {}
            file => file.and_then(|file| file.sync_all()).map_err(fail)?,
        }
        if stop == Some(path) {
            break;
        }
    }
    Ok(())
}

/// Does nothing on Windows, as the tape's `sync_dir` does: NTFS journals directory entries.
#[cfg(not(unix))]
fn sync_dirs(
    dir: &Path,
    data_dir: &Path,
    stop: Option<&Path>,
    holder: Option<&Path>,
) -> Result<(), String> {
    let _ = (dir, data_dir, stop, holder);
    Ok(())
}

/// Writes a complete config to a temp file, then hard-links it to `config`, so `config`
/// never appears empty or partial. A concurrent creator winning the race is fine. A temp
/// file left by a killed run is ignored.
fn publish(dir: &Path, config: &Path) -> Result<(), String> {
    let hex = crate::random_hex(8).map_err(crate::platform::no_random)?;
    let tmp = dir.join(format!(".atticdb.json.{hex}"));
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
