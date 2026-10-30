//! How `atticdb mcp` reaches a server: the operator's `--url`, a server found through
//! `<data_dir>/LOCK` on its Unix socket, or a server it starts itself.
//!
//! Discovery never uses the TCP address in `LOCK`, and never sends the token to a
//! socket that is not ours, under an owner-only parent, answering with the `LOCK`
//! instance id.

use crate::http::{self, Stream};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FIRST_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

/// A server address an operator chose with `--url`.
#[derive(Clone, Debug)]
pub enum Url {
    /// `unix:///path`.
    Unix(PathBuf),
    /// `http://host:port`. The authority is also the `Host` header.
    Tcp(String),
}

impl Url {
    /// Parses `unix:///path` or `http://<loopback host>:<port>`. The token travels in
    /// cleartext, so `http` takes only 127.0.0.1, localhost or [::1].
    pub fn parse(text: &str) -> Result<Self, String> {
        if let Some(path) = text.strip_prefix("unix://").filter(|path| path.starts_with('/')) {
            return Ok(Self::Unix(path.into()));
        }
        match text.strip_prefix("http://").map(|rest| rest.trim_end_matches('/')) {
            Some(authority) if crate::api::loopback_host(authority.as_bytes()) => {
                // Without a port every connect would fail until the deadline.
                if !authority.trim_start_matches("[::1]").contains(':') {
                    return Err(format!("url {text} needs an explicit :<port>"));
                }
                Ok(Self::Tcp(authority.into()))
            }
            _ => Err("url must be unix:///path or http://<127.0.0.1, localhost or [::1]>:<port>; \
                      http sends the token in cleartext"
                .into()),
        }
    }
}

/// Connects to a server, starting one when none is running.
#[derive(Debug)]
pub struct Launch {
    url: Option<Url>,
    data_dir: Option<PathBuf>,
    config: Option<PathBuf>,
    unix_socket: Option<PathBuf>,
    child: Option<Child>,
    next_spawn: Instant,
    backoff: Duration,
    /// The last reason `discover` gave, for the timeout error and to print each one once.
    refused: String,
    /// Set when the last `connect` hit a start failure that a retry within the same
    /// call cannot fix. The next call tries again.
    stuck: bool,
}

impl Launch {
    /// `data_dir`, `config` and `unix_socket` are absolute. Without `url` `data_dir` is
    /// required, and `config` lets it start a server. `unix_socket` is the config's, which the
    /// started server gets as a flag.
    pub fn new(
        url: Option<Url>,
        data_dir: Option<PathBuf>,
        config: Option<PathBuf>,
        unix_socket: Option<PathBuf>,
    ) -> Self {
        let refused = String::new();
        Self {
            url,
            data_dir,
            config,
            unix_socket,
            child: None,
            next_spawn: Instant::now(),
            backoff: FIRST_BACKOFF,
            refused,
            stuck: false,
        }
    }

    /// Opens a connection and returns it with the `Host` header to send. Waits for a
    /// server until `deadline`. A spawned server keeps starting after that, so a later
    /// call finds it.
    pub fn connect(&mut self, deadline: Instant) -> io::Result<(Box<dyn Stream>, String)> {
        let dir = match (&self.url, &self.data_dir) {
            (Some(Url::Unix(path)), _) => {
                return Ok((Box::new(connect_unix(path, deadline)?), "localhost".into()))
            }
            (Some(Url::Tcp(authority)), _) => {
                return Ok((Box::new(connect_tcp(authority, deadline)?), authority.clone()))
            }
            (None, Some(dir)) => dir.clone(),
            (None, None) => return Err(io::Error::other("no --url and no data directory")),
        };
        self.stuck = false;
        let log = dir.join("serve.log");
        loop {
            if let Some(Ok(Some(status))) = self.child.as_mut().map(Child::try_wait) {
                eprintln!("atticdb: started server exited: {status}");
                self.child = None;
                // 75 lost the lock race and 70 is a poison exit: both clear on a retry.
                // Anything else is a start failure, such as a bad data directory.
                if !matches!(status.code(), Some(75 | 70) | None) {
                    self.stuck = true;
                    let why = last_line(&log);
                    return Err(io::Error::other(format!(
                        "started server exited ({status}): {why}"
                    )));
                }
            }
            match discover(&dir, deadline) {
                Ok(stream) => {
                    (self.backoff, self.refused) = (FIRST_BACKOFF, String::new());
                    return Ok((Box::new(stream), "localhost".into()));
                }
                // A server that is not up yet is the normal first run, so it stays quiet.
                Err(refusal) => {
                    let reason = refusal.to_string();
                    if matches!(refusal, Refusal::Untrusted(_)) && reason != self.refused {
                        eprintln!("atticdb: no trusted server: {reason}");
                    }
                    self.refused = reason;
                }
            }
            // A child that exited 75 lost the lock race, but the holder may be a poisoned
            // server shutting down. So spawn again, with backoff, until one is trusted.
            if self.child.is_none() && Instant::now() >= self.next_spawn {
                if let Some(config) = self.config.clone() {
                    self.spawn(&dir, &config).inspect_err(|_| self.stuck = true)?;
                }
            }
            if Instant::now() >= deadline {
                let (why, log) = (&self.refused, log.display());
                let message = format!("no trusted server in time ({why}), see {log}");
                return Err(io::Error::new(ErrorKind::TimedOut, message));
            }
            std::thread::sleep(POLL);
        }
    }

    /// Whether the last `connect` failed in a way a retry within the same call cannot fix.
    pub fn stuck(&self) -> bool {
        self.stuck
    }

    /// `atticdb serve` on the Unix socket only, outliving this adapter. The token stays
    /// in the config, never on the command line or in the environment.
    fn spawn(&mut self, dir: &Path, config: &Path) -> io::Result<()> {
        crate::listen::owned_dir(dir, 0o077).map_err(io::Error::other)?;
        let path = dir.join("serve.log");
        let log = crate::config::open_regular(
            OpenOptions::new().create(true).append(true).mode(0o600),
            &path,
        )?;
        crate::config::check_private(&log, &path, "server log").map_err(io::Error::other)?;
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("serve").arg("--config").arg(config).arg("--data-dir").arg(dir);
        if let Some(socket) = &self.unix_socket {
            command.arg("--unix-socket").arg(socket);
        }
        let child = command
            .args(["--listen", "none", "--exit-on-poison"])
            .env_remove(crate::mcp::TOKEN_ENV)
            // Every path it gets is absolute, so it need not pin the first chat's folder.
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()?;
        eprintln!("atticdb: started server pid {}", child.id());
        self.child = Some(child);
        self.next_spawn = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
        Ok(())
    }
}

impl Drop for Launch {
    // A started server outlives its `Launch`. A thread waits for it, so a process that runs
    // on, such as the studio, reaps it when it exits instead of keeping a zombie.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = std::thread::Builder::new().name("atticdb-reap".into()).spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// The last non-empty line in the log's final 4 KiB, or a pointer to the log.
fn last_line(path: &Path) -> String {
    let mut tail = Vec::new();
    if let Ok(mut file) = crate::config::open_regular(OpenOptions::new().read(true), path) {
        let len = file.metadata().map_or(0, |meta| meta.len());
        let _ = file.seek(SeekFrom::Start(len.saturating_sub(4096)));
        let _ = file.take(4096).read_to_end(&mut tail);
    }
    match String::from_utf8_lossy(&tail).lines().rev().find(|line| !line.trim().is_empty()) {
        Some(line) => line.trim().to_owned(),
        None => format!("see {}", path.display()),
    }
}

fn connect_tcp(authority: &str, deadline: Instant) -> io::Result<TcpStream> {
    let mut error = io::Error::new(ErrorKind::NotFound, format!("{authority} has no address"));
    for addr in loopback(authority.to_socket_addrs()?.collect())? {
        match TcpStream::connect_timeout(&addr, http::remaining(deadline)?) {
            Ok(stream) => return Ok(stream),
            Err(next) => error = next,
        }
    }
    Err(error)
}

/// The resolved addresses of an `http` URL, refused unless all are loopback: the token
/// travels in cleartext, and a name such as `localhost` could resolve anywhere.
fn loopback(addrs: Vec<SocketAddr>) -> io::Result<Vec<SocketAddr>> {
    match addrs.iter().find(|addr| !addr.ip().is_loopback()) {
        Some(addr) => Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("{addr} is not loopback; http sends the token in cleartext"),
        )),
        None => Ok(addrs),
    }
}

/// Connects by `deadline`. A blocking connect waits while the listen backlog is full;
/// a nonblocking one fails with `EAGAIN` instead, so it is retried until the deadline.
pub(crate) fn connect_unix(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};
    let socket = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
    rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC)?;
    rustix::io::ioctl_fionbio(&socket, true)?;
    let addr = SocketAddrUnix::new(path)?;
    while let Err(error) = rustix::net::connect(&socket, &addr) {
        if error != rustix::io::Errno::AGAIN {
            return Err(error.into());
        }
        std::thread::sleep(POLL.min(http::remaining(deadline)?));
    }
    rustix::io::ioctl_fionbio(&socket, false)?;
    Ok(UnixStream::from(socket))
}

/// Why [`discover`] found no server to trust.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No server is up yet: no `LOCK`, no address in it, or nothing listening on its socket.
    NotRunning(String),
    /// Something is there but failed a trust check, or another error got in the way.
    Untrusted(String),
}

impl Refusal {
    /// `NotRunning` when `error` is one of `kinds`, else `Untrusted`. Both carry `reason`.
    fn of(error: &io::Error, kinds: &[ErrorKind], reason: String) -> Self {
        if kinds.contains(&error.kind()) {
            Self::NotRunning(reason)
        } else {
            Self::Untrusted(reason)
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning(reason) | Self::Untrusted(reason) => f.write_str(reason),
        }
    }
}

/// A connection to the socket named in `LOCK`, if it is ours, under an owner-only parent,
/// and answered `/healthz` on this connection with the instance id in `LOCK`. The token
/// then goes over the same connection. Otherwise the reason, and no token was sent.
pub(crate) fn discover(dir: &Path, deadline: Instant) -> Result<UnixStream, Refusal> {
    const MISSING: &[ErrorKind] = &[ErrorKind::NotFound];
    let mut lock = String::new();
    crate::config::open_regular(OpenOptions::new().read(true), &dir.join("LOCK"))
        .and_then(|mut file| file.read_to_string(&mut lock))
        .map_err(|e| Refusal::of(&e, MISSING, format!("LOCK: {e}")))?;
    // `pid=<pid> instance=<id> unix=<path> tcp=<addr>`. The path may hold spaces.
    let fields = lock.trim_end().rsplit_once(" tcp=").map(|(head, _)| head);
    let instance =
        fields.and_then(|head| head.split(' ').find_map(|field| field.strip_prefix("instance=")));
    let socket = fields.and_then(|head| head.split_once(" unix=")).map(|(_, path)| path);
    let (Some(instance), Some(socket)) = (instance, socket.map(Path::new)) else {
        return Err(Refusal::NotRunning("LOCK names no server".into()));
    };
    let missing = |error: io::Error| {
        Refusal::of(&error, MISSING, format!("socket {}: {error}", socket.display()))
    };
    // Resolve symlinks first, so the checks below cover the directories actually used.
    let parent = socket.parent().map(std::fs::canonicalize).transpose().map_err(missing)?;
    let socket = parent.unwrap_or_default().join(socket.file_name().unwrap_or_default());
    let euid = rustix::process::geteuid().as_raw();
    let missing = |error: io::Error| {
        Refusal::of(&error, MISSING, format!("socket {}: {error}", socket.display()))
    };
    let meta = std::fs::symlink_metadata(&socket).map_err(missing)?;
    if !meta.file_type().is_socket() || meta.uid() != euid {
        return Err(Refusal::Untrusted(format!(
            "{} must be a socket owned by uid {euid}, found owner uid {}",
            socket.display(),
            meta.uid()
        )));
    }
    let mut dirs = socket.ancestors().skip(1);
    crate::listen::owned_dir(dirs.next().unwrap_or(Path::new("/")), 0o022)
        .map_err(Refusal::Untrusted)?;
    // OpenSSH StrictModes: anyone who can rename a directory above the socket can swap it.
    for dir in dirs {
        let meta = std::fs::metadata(dir)
            .map_err(|error| Refusal::Untrusted(format!("{}: {error}", dir.display())))?;
        let writable = meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0;
        if (meta.uid() != 0 && meta.uid() != euid) || writable {
            return Err(Refusal::Untrusted(format!(
                "{} must be owned by root or uid {euid} and not writable by group or others \
                 unless sticky, found mode {:04o} owned by uid {}",
                dir.display(),
                meta.mode() & 0o7777,
                meta.uid()
            )));
        }
    }
    // A refused connect means a stale socket that nobody listens on. A server binds and
    // listens before it links the socket here, so a starting server never refuses.
    let idle = [ErrorKind::ConnectionRefused, ErrorKind::NotFound];
    let fail = |error: &io::Error| format!("{}: {error}", socket.display());
    let mut stream =
        connect_unix(&socket, deadline).map_err(|e| Refusal::of(&e, &idle, fail(&e)))?;
    let headers = [("Host", "localhost"), ("Connection", "keep-alive")];
    let deadline = deadline.min(Instant::now() + PROBE_TIMEOUT);
    let response = http::exchange(&mut stream, "GET", "/healthz", &headers, &[], deadline, 4096);
    let response = response.map_err(|error| Refusal::Untrusted(fail(&error)))?;
    let health: Value = serde_json::from_slice(&response.body).unwrap_or_default();
    if matches!(response.status, 200 | 503) && health["instance"] == instance {
        Ok(stream)
    } else {
        Err(Refusal::Untrusted(format!(
            "{} is not instance {instance} from LOCK",
            socket.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::Refusal;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// A private 0700 directory under `/tmp`, short enough for a socket path. Removed on drop.
    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name = format!("atticdb-unit-{}-{next}", std::process::id());
            // The real path, since `discover` resolves the socket's parent.
            let path = std::fs::canonicalize("/tmp").unwrap().join(name);
            std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn lock(dir: &Path, socket: &Path) {
        let text = format!("pid=1 instance=0123 unix={} tcp=none\n", socket.display());
        std::fs::write(dir.join("LOCK"), text).unwrap();
    }

    fn discover(dir: &Path) -> Refusal {
        let deadline = Instant::now() + Duration::from_secs(5);
        super::discover(dir, deadline).unwrap_err()
    }

    /// Only "no server is up yet" is `NotRunning`. A failed trust check is `Untrusted`.
    #[test]
    fn discover_tells_a_missing_server_from_an_untrusted_one() {
        let temp = Temp::new();
        let dir = &temp.0;
        let refusal = discover(dir);
        assert!(
            matches!(&refusal, Refusal::NotRunning(r) if r.starts_with("LOCK: ")),
            "{refusal:?}"
        );

        std::fs::write(dir.join("LOCK"), "pid=1\n").unwrap();
        assert_eq!(discover(dir), Refusal::NotRunning("LOCK names no server".into()));

        let socket = dir.join("s");
        lock(dir, &socket);
        let refusal = discover(dir);
        assert!(
            matches!(&refusal, Refusal::NotRunning(r) if r.starts_with("socket ")),
            "{refusal:?}"
        );

        // A stale socket: the file is there but nobody listens, so a connect is refused.
        drop(UnixListener::bind(&socket).unwrap());
        assert!(matches!(discover(dir), Refusal::NotRunning(_)), "{:?}", discover(dir));
        std::fs::remove_file(&socket).unwrap();

        // Something that is not a socket.
        std::fs::write(&socket, "").unwrap();
        let refusal = discover(dir);
        assert!(
            matches!(&refusal, Refusal::Untrusted(r) if r.contains("must be a socket")),
            "{refusal:?}"
        );
        std::fs::remove_file(&socket).unwrap();

        // Our socket, live, but in a directory others may write to.
        let open = dir.join("g");
        std::fs::DirBuilder::new().mode(0o700).create(&open).unwrap();
        let socket = open.join("s");
        let _listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o770)).unwrap();
        lock(dir, &socket);
        let refusal = discover(dir);
        let reason = "not writable by group or others";
        assert!(matches!(&refusal, Refusal::Untrusted(r) if r.contains(reason)), "{refusal:?}");
    }

    #[test]
    fn http_connects_only_to_loopback_addresses() {
        for (addrs, ok) in [
            (&["127.0.0.1:1", "[::1]:1"][..], true),
            (&["127.0.0.1:1", "10.0.0.1:1"], false),
            (&["[::ffff:192.0.2.1]:1"], false),
        ] {
            let result = super::loopback(addrs.iter().map(|addr| addr.parse().unwrap()).collect());
            assert_eq!(result.is_ok(), ok, "{addrs:?}: {result:?}");
        }
    }
}
