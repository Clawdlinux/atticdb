//! TCP and Unix socket listeners, with the checks that keep the lock race and stale
//! sockets safe. The Unix socket is installed only by the process that holds the lock.

use std::fs::{self, DirBuilder, Permissions};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

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

/// Binds a loopback TCP address. Any other address is refused.
pub fn bind_tcp(addr: SocketAddr) -> Result<TcpListener, String> {
    if !addr.ip().is_loopback() {
        return Err(NEEDS_TLS.into());
    }
    TcpListener::bind(addr).map_err(|error| format!("cannot listen on {addr}: {error}"))
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

/// Checks a socket's directory: owned by the effective uid, not writable by group or others.
pub fn check_parent(parent: &Path) -> Result<(), String> {
    let euid = rustix::process::geteuid().as_raw();
    let meta = fs::metadata(parent)
        .map_err(|error| format!("socket directory {}: {error}", parent.display()))?;
    if !meta.is_dir() || meta.uid() != euid || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "socket directory {} must be a directory owned by uid {euid} and not writable by group or others, \
             found mode {:04o} owned by uid {}",
            parent.display(),
            meta.mode() & 0o7777,
            meta.uid()
        ));
    }
    Ok(())
}

/// Creates a data directory with mode 0700, or checks that an existing one is ours with
/// no group or other bits.
pub fn private_dir(dir: &Path) -> Result<(), String> {
    let fail = |error: std::io::Error| format!("data directory {}: {error}", dir.display());
    DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(fail)?;
    let meta = fs::metadata(dir).map_err(fail)?;
    let euid = rustix::process::geteuid().as_raw();
    if !meta.is_dir() || meta.uid() != euid || meta.mode() & 0o077 != 0 {
        return Err(format!(
            "data directory {} must be owned by uid {euid} with mode 0700 or stricter, \
             found mode {:04o} owned by uid {}",
            dir.display(),
            meta.mode() & 0o7777,
            meta.uid()
        ));
    }
    Ok(())
}

/// Installs the Unix socket at `path`. Call it only while holding the data directory lock.
///
/// It binds inside a private 0700 staging directory named after this start's `instance`,
/// sets the mode (0600, or 0660 with `group`), then hard-links the socket onto `path`. A
/// link fails if `path` exists, so an existing path is never clobbered, and nobody else
/// can connect before the mode is set. Other staging directories are never touched.
pub fn install_unix(
    path: &Path,
    group: Option<u32>,
    instance: &str,
) -> Result<UnixListener, String> {
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
    check_parent(parent)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(format!("socket {}: {error}", path.display())),
        Ok(meta) if !meta.file_type().is_socket() => {
            return Err(format!("{} exists and is not a socket", path.display()))
        }
        Ok(_) => match UnixStream::connect(path) {
            Ok(_) => return Err(live(path)),
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
        Ok(listener)
    })();
    let _ = fs::remove_file(&staged);
    let _ = fs::remove_dir(&staging);
    installed.map_err(|error: std::io::Error| match UnixStream::connect(path) {
        // Another server on another data directory linked the same path first.
        Ok(_) if error.kind() == ErrorKind::AlreadyExists => live(path),
        _ => format!("cannot install socket {}: {error}", path.display()),
    })
}

fn live(path: &Path) -> String {
    format!("another server is listening on {}", path.display())
}
