//! The per-user home that `zerodb mcp` and the shell verbs use when given no connection
//! flags: an owner-only directory with `zerodb.json` and `data/`, created on first use.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use zerodb_tape::{Io, StdIo};

/// The tenant the home's config names.
pub const TENANT: &str = "me";
/// The variable that moves the home. Empty counts as unset.
pub const HOME_ENV: &str = "ZERODB_HOME";
/// Most bytes read from a `.git` or `commondir` file. A longer one names no real path.
const MAX_GIT_FILE: u64 = 8 << 10;

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
/// and `<dir>/commondir` names the common git dir. A common dir named `.git` gives its
/// parent's name. Any other gives its own name, without a `.git` suffix. `None` for a
/// `.git` directory, a file without `commondir` such as a submodule's, or anything
/// unreadable.
fn main_repo(git: &Path) -> Option<String> {
    let git_dir = path_in(git, "gitdir: ")?;
    let common = std::fs::canonicalize(path_in(&git_dir.join("commondir"), "")?).ok()?;
    let name = common.file_name()?.to_str()?;
    if name == ".git" {
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
    Some(file.parent()?.join(OsStr::from_bytes(&path[..=end])))
}

/// `$HOME`, which must be absolute.
pub fn user_home() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
    home.ok_or_else(|| "HOME must be set to an absolute path".into())
}

/// `$ZERODB_HOME` when it is set and not empty. A relative path is an error, so nothing
/// falls back to the default home.
pub fn env_dir() -> Result<Option<PathBuf>, String> {
    match std::env::var_os(HOME_ENV).filter(|dir| !dir.is_empty()).map(PathBuf::from) {
        Some(dir) if !dir.is_absolute() => Err(format!("{HOME_ENV} must be an absolute path")),
        dir => Ok(dir),
    }
}

/// The home directory: `$ZERODB_HOME` when set, by [`env_dir`]. Else on macOS
/// `$HOME/Library/Application Support/zerodb`. On Linux `$XDG_DATA_HOME/zerodb` when that
/// is set and absolute, else `$HOME/.local/share/zerodb`.
pub fn dir() -> Result<PathBuf, String> {
    if let Some(dir) = env_dir()? {
        return Ok(dir);
    }
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
    // Stop at $HOME, or at the home's grandparent when the home is outside $HOME. That is
    // where XDG_DATA_HOME or ZERODB_HOME may put it. ZERODB_HOME works with no $HOME.
    let home = user_home().ok().filter(|home| dir.starts_with(home));
    let top = home.as_deref().unwrap_or_else(|| dir.ancestors().nth(2).unwrap_or(Path::new("/")));
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    /// Each row lays out files in its own folder, then names the repo of 1 directory there.
    #[test]
    fn repo_names_a_worktree_after_its_main_repo() {
        let scratch = std::env::temp_dir().join(format!("zerodb-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        // (case, files with their text, the directory, its repo name). A path ending in `/`
        // is a directory. `{dir}` in a text is the row's folder.
        let rows = [
            ("plain repo", vec![("app/.git/", "")], "app/src", "app"),
            (
                "worktree, relative commondir",
                vec![
                    ("app/.git/worktrees/wt/commondir", "../..\n"),
                    ("wt/.git", "gitdir: ../app/.git/worktrees/wt\n"),
                ],
                "wt/src",
                "app",
            ),
            (
                "worktree, absolute commondir",
                vec![
                    ("app/.git/worktrees/wt/commondir", "{dir}/app/.git\n"),
                    ("wt/.git", "gitdir: {dir}/app/.git/worktrees/wt\n"),
                ],
                "wt",
                "app",
            ),
            (
                "submodule",
                vec![
                    ("app/.git/modules/library/", ""),
                    ("app/lib/.git", "gitdir: ../.git/modules/library\n"),
                ],
                "app/lib",
                "lib",
            ),
            (
                "malformed .git file",
                vec![
                    ("app/.git/worktrees/wt/commondir", "../..\n"),
                    ("wt/.git", "../app/.git/worktrees/wt\n"),
                ],
                "wt",
                "wt",
            ),
            (
                "common dir gone",
                vec![
                    ("app/.git/worktrees/wt/commondir", "{dir}/gone/.git\n"),
                    ("wt/.git", "gitdir: {dir}/app/.git/worktrees/wt\n"),
                ],
                "wt",
                "wt",
            ),
            (
                "bare common dir",
                vec![
                    ("app.git/worktrees/wt/commondir", "../..\n"),
                    ("wt/.git", "gitdir: {dir}/app.git/worktrees/wt\n"),
                ],
                "wt",
                "app",
            ),
            ("no .git", vec![], "app/src", "src"),
        ];
        for (n, (case, files, dir, name)) in rows.into_iter().enumerate() {
            let folder = scratch.join(n.to_string());
            for (path, text) in files {
                match path.strip_suffix('/') {
                    Some(path) => std::fs::create_dir_all(folder.join(path)).unwrap(),
                    None => {
                        let path = folder.join(path);
                        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                        let text = text.replace("{dir}", folder.to_str().unwrap());
                        std::fs::write(path, text).unwrap();
                    }
                }
            }
            std::fs::create_dir_all(folder.join(dir)).unwrap();
            assert_eq!(super::repo(&folder.join(dir)).as_deref(), Some(name), "{case}");
        }
        std::fs::remove_dir_all(&scratch).unwrap();
    }
}
