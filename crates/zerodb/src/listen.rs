//! TCP and Unix socket listeners, with the checks that keep the lock race and stale
//! sockets safe. The Unix socket is installed only by the process that holds the lock.

use rustix::fs::{FlockOperation, Mode, OFlags};
use std::fs::{self, DirBuilder, Permissions};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The default TCP port, on 127.0.0.1.
pub const DEFAULT_PORT: u16 = 7425;
/// The default socket file name inside the data directory.
pub const SOCKET_NAME: &str = "zerodb.sock";
/// The refusal for a non-loopback TCP address.
pub const NEEDS_TLS: &str = "non-loopback listening needs TLS, which zerodb does not support yet; \
                             put a TLS proxy on loopback in front";
/// `sun_path` size, terminating NUL included.
pub const SUN_PATH_BYTES: usize = if cfg!(target_os = "linux") { 108 } else { 104 };
const STAGING: &str = ".zs.";
const PROBE: Duration = Duration::from_secs(1);

/// Binds a loopback TCP address. Any other address is refused.
pub fn bind_tcp(addr: SocketAddr) -> std::io::Result<TcpListener> {
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::other(NEEDS_TLS));
    }
    TcpListener::bind(addr).map_err(|error| {
        std::io::Error::new(error.kind(), format!("cannot listen on {addr}: {error}"))
    })
}

/// Resolves `--unix-socket-group`: a numeric gid, or a name from `/etc/group`.
pub fn resolve_group(group: &str) -> Result<u32, String> {
    if let Ok(gid) = group.parse() {
        return Ok(gid);
    }
    let groups = fs::read_to_string("/etc/group").unwrap_or_default();
    groups
        .lines()
        .map(|line| line.split(':').collect::<Vec<_>>())
        .find(|fields| fields.len() > 2 && fields[0] == group)
        .and_then(|fields| fields[2].parse().ok())
        .ok_or_else(|| format!("group {group:?} is not in /etc/group; pass its numeric gid"))
}

/// Checks that `dir` is a directory owned by the effective uid with none of the `mask`
/// mode bits: 0o022 for a socket's directory, 0o077 for a data directory, which is
/// first created with mode 0700 if missing.
pub fn owned_dir(dir: &Path, mask: u32) -> Result<(), String> {
    let fail = |error: std::io::Error| format!("{}: {error}", dir.display());
    if mask == 0o077 {
        DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(fail)?;
    }
    check_owned(dir, mask)
}

/// The check of [`owned_dir`], on an existing `dir`. It creates nothing.
pub fn check_owned(dir: &Path, mask: u32) -> Result<(), String> {
    let fail = |error: std::io::Error| format!("{}: {error}", dir.display());
    let meta = fs::metadata(dir).map_err(fail)?;
    let euid = rustix::process::geteuid().as_raw();
    if !meta.is_dir() || meta.uid() != euid || meta.mode() & mask != 0 {
        let rule =
            if mask == 0o077 { "mode 0700 or stricter" } else { "not writable by group or others" };
        return Err(format!(
            "{} must be a directory owned by uid {euid}, {rule}, found mode {:04o} owned by uid {}",
            dir.display(),
            meta.mode() & 0o7777,
            meta.uid()
        ));
    }
    Ok(())
}

/// Installs the Unix socket at `path`. Call it only while holding the data directory lock.
///
/// Every change to `path` happens under an exclusive `flock` on `<path>.lock`, so servers on
/// other data directories that share the path take turns. It binds inside a private 0700
/// staging directory named after this start's `instance`, sets the mode (0600, or 0660 with
/// `group`), then hard-links the socket onto `path`. A live socket at `path` fails, a stale
/// one is removed first. Nobody else can connect before the mode is set.
pub fn install_unix(
    path: &Path,
    group: Option<u32>,
    instance: &str,
) -> Result<(UnixListener, Installed), String> {
    let path = &std::path::absolute(path).map_err(|error| format!("socket path: {error}"))?;
    let parent = path.parent().unwrap_or(Path::new("/"));
    let staging = parent.join(format!("{STAGING}{}", instance.get(..8).unwrap_or(instance)));
    let staged = staging.join("s");
    for checked in [path, &staged] {
        let bytes = checked.as_os_str().len() + 1;
        if bytes > SUN_PATH_BYTES {
            return Err(format!(
                "socket path {} needs {bytes} bytes, over the {SUN_PATH_BYTES}-byte sun_path limit; \
                 set --unix-socket to a shorter path",
                checked.display()
            ));
        }
    }
    owned_dir(parent, 0o022)?;
    let _lock =
        lock(path).map_err(|error| format!("cannot lock {}.lock: {error}", path.display()))?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(format!("socket {}: {error}", path.display())),
        Ok(meta) if !meta.file_type().is_socket() => {
            return Err(format!("{} exists and is not a socket", path.display()))
        }
        // Bounded, so a listener with a full backlog cannot hang startup under the lock.
        // Only a refused connect proves the socket stale. A timeout counts as live.
        Ok(_) => match crate::launch::connect_unix(path, Instant::now() + PROBE) {
            Ok(_) => return Err(format!("another server is listening on {}", path.display())),
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                return Err(format!("another server is listening on {}", path.display()))
            }
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => fs::remove_file(path)
                .map_err(|error| {
                    format!("cannot remove stale socket {}: {error}", path.display())
                })?,
            Err(error) => return Err(format!("cannot probe socket {}: {error}", path.display())),
        },
    }
    DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|error| format!("cannot create {}: {error}", staging.display()))?;
    let installed = (|| {
        let listener = UnixListener::bind(&staged)?;
        if group.is_some() {
            std::os::unix::fs::chown(&staged, None, group)?;
        }
        fs::set_permissions(
            &staged,
            Permissions::from_mode(if group.is_some() { 0o660 } else { 0o600 }),
        )?;
        fs::hard_link(&staged, path)?;
        let meta = fs::symlink_metadata(path)?;
        Ok((listener, Installed { path: path.clone(), id: (meta.dev(), meta.ino()) }))
    })();
    let _ = fs::remove_file(&staged);
    let _ = fs::remove_dir(&staging);
    installed.map_err(|error: std::io::Error| {
        format!("cannot install socket {}: {error}", path.display())
    })
}

/// A socket path this server installed, and the file it linked there.
#[derive(Clone, Debug)]
pub struct Installed {
    /// Where clients connect.
    pub path: PathBuf,
    id: (u64, u64),
}

impl Installed {
    /// Removes the path, under the same lock as the install, if it is still our socket.
    /// A socket another server installed there later stays.
    pub fn remove(&self) -> std::io::Result<()> {
        let _lock = lock(&self.path)?;
        let meta = fs::symlink_metadata(&self.path)?;
        if (meta.dev(), meta.ino()) == self.id {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

/// An exclusive `flock` on `<path>.lock`, held until the descriptor drops.
fn lock(path: &Path) -> std::io::Result<OwnedFd> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let flags =
        OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let fd = rustix::fs::open(Path::new(&name), flags, Mode::from_raw_mode(0o600))?;
    if !rustix::fs::FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode).is_file() {
        return Err(std::io::Error::other(format!("{} must be a regular file", name.display())));
    }
    rustix::fs::flock(&fd, FlockOperation::LockExclusive)?;
    Ok(fd)
}
