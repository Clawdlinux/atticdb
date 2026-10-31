//! How `atticdb mcp` reaches a server: the operator's `--url`, a server found through
//! `<data_dir>/LOCK` on its Unix socket, or a server it starts itself.
//!
//! On Unix, discovery never uses the TCP address in `LOCK`. It sends the token only to a
//! socket that is ours, under an owner-only parent. That socket must answer with the `LOCK`
//! instance id.
//!
//! Windows has no Unix socket here, so discovery there does use the TCP address in `LOCK`.
//! A started server listens on an ephemeral loopback port, which `LOCK` names. Discovery
//! connects to that port and sends `/healthz` with a random nonce. The token goes over that
//! connection only after a proof. The server must show it knows the token, for that nonce,
//! the `LOCK` instance id and the address connected to.

#[cfg(any(windows, test))]
use crate::config::Token;
use crate::http::{self, Stream};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FIRST_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);
/// Longest TCP connect in discovery. Windows takes about 2 s to report a refused one.
#[cfg(any(windows, test))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Smallest tenant entry in a config: `{"name":"a","token":"<32 bytes>"},` is 56 bytes.
#[cfg(any(windows, test))]
const MIN_TENANT_JSON: u64 = 56;
/// Most bytes of a `/healthz` body with proofs. A config of at most 1 MiB holds at most
/// 1 MiB / 56 tenants. Each proof takes 67 bytes: 64 hex digits, 2 quotes and a comma.
/// 4 KiB covers the other fields. That is about 1.2 MiB.
#[cfg(any(windows, test))]
const PROOF_BYTES: u64 = crate::config::MAX_FILE_BYTES / MIN_TENANT_JSON * 67 + 4096;
/// Why a `unix://` URL fails where there is no Unix socket.
#[cfg(windows)]
const NO_UNIX: &str =
    "unix:// needs a Unix socket, which Windows lacks; use http://127.0.0.1:<port>";

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
        #[cfg(windows)]
        if text.starts_with("unix://") {
            return Err(NO_UNIX.into());
        }
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
    /// The token a discovered server must prove it knows, before it gets the token.
    #[cfg(windows)]
    token: Option<Token>,
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
            #[cfg(windows)]
            token: None,
        }
    }

    /// Sets the token a server found through `LOCK` must prove it knows.
    #[cfg(windows)]
    pub fn with_token(mut self, token: Token) -> Self {
        self.token = Some(token);
        self
    }

    /// Opens a connection and returns it with the `Host` header to send. Waits for a
    /// server until `deadline`. A spawned server keeps starting after that, so a later
    /// call finds it.
    pub fn connect(&mut self, deadline: Instant) -> io::Result<(Box<dyn Stream>, String)> {
        let dir = match (&self.url, &self.data_dir) {
            #[cfg(unix)]
            (Some(Url::Unix(path)), _) => {
                return Ok((Box::new(connect_unix(path, deadline)?), "localhost".into()))
            }
            #[cfg(windows)]
            (Some(Url::Unix(_)), _) => return Err(io::Error::new(ErrorKind::Unsupported, NO_UNIX)),
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
            #[cfg(unix)]
            let found = discover(&dir, deadline);
            #[cfg(windows)]
            let found = discover(&dir, deadline, self.token.as_ref());
            match found {
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
    #[cfg(unix)]
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

    /// `atticdb serve` on an ephemeral loopback port, outliving this adapter, with no console.
    /// `LOCK` names the port it got. The token stays in the config, never on the command
    /// line or in the environment.
    #[cfg(windows)]
    fn spawn(&mut self, dir: &Path, config: &Path) -> io::Result<()> {
        if self.unix_socket.is_some() {
            return Err(io::Error::new(ErrorKind::Unsupported, "unix_socket: Windows has none"));
        }
        crate::listen::owned_dir(dir, 0o077).map_err(io::Error::other)?;
        let path = dir.join("serve.log");
        let log = crate::config::open_regular(OpenOptions::new().create(true).append(true), &path)?;
        crate::config::check_private(&log, &path, "server log").map_err(io::Error::other)?;
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("serve").arg("--config").arg(config).arg("--data-dir").arg(dir);
        command
            .args(["--listen", "127.0.0.1:0", "--exit-on-poison"])
            .env_remove(crate::mcp::TOKEN_ENV)
            // Every path it gets is absolute, so it need not pin the first chat's folder.
            .current_dir(crate::platform::root_dir())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        crate::platform::detach(&mut command);
        let child = command.spawn()?;
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
#[cfg(unix)]
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
#[cfg(unix)]
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

/// A connection to the loopback address in `LOCK`, by [`discover_tcp`].
#[cfg(windows)]
pub(crate) fn discover(
    dir: &Path,
    deadline: Instant,
    token: Option<&Token>,
) -> Result<TcpStream, Refusal> {
    let token =
        token.ok_or_else(|| Refusal::Untrusted("no token to check a server with".into()))?;
    discover_tcp(dir, deadline, token)
}

/// A connection to the TCP address in `LOCK`, which must be 127.0.0.1 or [::1]. On this
/// connection the server must answer `/healthz` with the `LOCK` instance id. It must also
/// prove it knows `token`, by [`crate::auth::verify`]. The token then goes over the same
/// connection. Otherwise it returns the reason, and no token was sent. The probe carries
/// only a random nonce.
#[cfg(any(windows, test))]
pub(crate) fn discover_tcp(
    dir: &Path,
    deadline: Instant,
    token: &Token,
) -> Result<TcpStream, Refusal> {
    use std::net::{Ipv4Addr, Ipv6Addr};
    const MISSING: &[ErrorKind] = &[ErrorKind::NotFound];
    let mut lock = String::new();
    crate::config::open_regular(OpenOptions::new().read(true), &dir.join("LOCK"))
        .and_then(|mut file| file.read_to_string(&mut lock))
        .map_err(|e| Refusal::of(&e, MISSING, format!("LOCK: {e}")))?;
    // `pid=<pid> instance=<id> tcp=<addr>`.
    let fields = lock.trim_end().rsplit_once(" tcp=");
    let instance = fields
        .and_then(|(head, _)| head.split(' ').find_map(|field| field.strip_prefix("instance=")));
    let addr = fields.map(|(_, addr)| addr).filter(|addr| *addr != "none");
    let (Some(instance), Some(addr)) = (instance, addr) else {
        return Err(Refusal::NotRunning("LOCK names no server".into()));
    };
    let addr: SocketAddr = addr
        .parse()
        .map_err(|_| Refusal::Untrusted(format!("LOCK names {addr:?}, which is not an address")))?;
    if addr.ip() != Ipv4Addr::LOCALHOST && addr.ip() != Ipv6Addr::LOCALHOST {
        return Err(Refusal::Untrusted(format!("LOCK names {addr}, not 127.0.0.1 or [::1]")));
    }
    // Nothing listening is a stale `LOCK`. A server listens before it writes `LOCK`. Windows
    // retries a refused loopback connect for about 2 s, so a timeout counts as refused too.
    let idle = [ErrorKind::ConnectionRefused, ErrorKind::TimedOut];
    let fail = |error: &io::Error| format!("{addr}: {error}");
    let mut stream = http::remaining(deadline)
        .and_then(|left| TcpStream::connect_timeout(&addr, left.min(CONNECT_TIMEOUT)))
        .map_err(|e| Refusal::of(&e, &idle, fail(&e)))?;
    let _ = stream.set_nodelay(true);
    // The proof covers the server's end of this connection. A relay can't pass on a proof.
    let endpoint = stream.peer_addr().map_err(|error| Refusal::Untrusted(fail(&error)))?;
    let nonce = crate::random_hex(crate::auth::NONCE_BYTES)
        .map_err(|error| Refusal::Untrusted(crate::platform::no_random(error)))?;
    let nonce_bytes = crate::auth::parse_nonce(nonce.as_bytes())
        .ok_or_else(|| Refusal::Untrusted("bad nonce".into()))?;
    // No `Authorization` here: the token goes only to a server that proved it knows it.
    let headers = [
        ("Host", "localhost"),
        ("Connection", "keep-alive"),
        (crate::auth::NONCE_HEADER, nonce.as_str()),
    ];
    let deadline = deadline.min(Instant::now() + PROBE_TIMEOUT);
    let response =
        http::exchange(&mut stream, "GET", "/healthz", &headers, &[], deadline, PROOF_BYTES);
    let response = response.map_err(|error| Refusal::Untrusted(fail(&error)))?;
    let health: Value = serde_json::from_slice(&response.body).unwrap_or_default();
    if !matches!(response.status, 200 | 503) || health["instance"] != instance {
        return Err(Refusal::Untrusted(format!("{addr} is not instance {instance} from LOCK")));
    }
    let endpoint = endpoint.to_string();
    if !crate::auth::verify(token, &nonce_bytes, instance, &endpoint, &health["proofs"]) {
        return Err(Refusal::Untrusted(format!("{addr} gave no proof that it knows the token")));
    }
    Ok(stream)
}

#[cfg(all(test, unix))]
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

/// [`discover_tcp`], the Windows discovery, on every platform.
#[cfg(test)]
mod tcp_tests {
    #![allow(clippy::unwrap_used)]
    use super::{discover_tcp, Refusal};
    use crate::auth::{parse_nonce, proof, proof_key};
    use crate::config::Token;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// A new directory under the system temp dir. Removed on drop.
    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name = format!("atticdb-tcp-unit-{}-{next}", std::process::id());
            let path = std::env::temp_dir().join(name);
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn token() -> Token {
        Token::new("t".repeat(40)).unwrap()
    }

    fn lock(dir: &Path, tcp: &str) {
        std::fs::write(dir.join("LOCK"), format!("pid=1 instance=0123 tcp={tcp}\n")).unwrap();
    }

    fn discover(dir: &Path) -> Result<std::net::TcpStream, Refusal> {
        discover_tcp(dir, Instant::now() + Duration::from_secs(10), &token())
    }

    /// How the fake server answers `/healthz`.
    #[derive(Clone, Copy)]
    enum Answer {
        Proof,
        WrongProof,
        NoProof,
        WrongInstance,
        /// The proof a server on another address would give, as a relay passes on.
        Relayed,
        /// The right proof last, after 99 others: more than 4 KiB of proofs.
        Many,
    }

    /// A server on an ephemeral loopback port that answers 1 request. Returns its address
    /// and a thread that yields the request bytes it got.
    fn fake(answer: Answer) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let at = addr.clone();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut seen = Vec::new();
            let mut chunk = [0; 4096];
            while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return seen,
                    Ok(n) => seen.extend_from_slice(&chunk[..n]),
                }
            }
            let text = String::from_utf8_lossy(&seen).to_ascii_lowercase();
            let nonce = text.lines().find_map(|line| line.strip_prefix("x-atticdb-nonce: "));
            let nonce = parse_nonce(nonce.unwrap_or_default().trim().as_bytes()).unwrap();
            let key = proof_key(&token());
            let right = proof(&key, &nonce, "0123", &at);
            let proofs = match answer {
                Answer::Proof | Answer::WrongInstance => vec![right],
                Answer::WrongProof => vec![proof(&key, &[0; 32], "0123", &at)],
                Answer::NoProof => vec![],
                Answer::Relayed => vec![proof(&key, &nonce, "0123", "127.0.0.1:1")],
                Answer::Many => {
                    let other = |n: u8| {
                        proof(
                            &proof_key(&Token::new("u".repeat(40)).unwrap()),
                            &[n; 32],
                            "0123",
                            &at,
                        )
                    };
                    (0..99).map(other).chain([right]).collect()
                }
            };
            let proofs: Vec<String> = proofs.iter().map(|hash| hash.to_hex().to_string()).collect();
            let instance = if matches!(answer, Answer::WrongInstance) { "9999" } else { "0123" };
            let body = serde_json::json!({"status": "ok", "instance": instance, "proofs": proofs});
            let body = body.to_string();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            // Anything sent after the answer, such as a token, would land here too.
            stream.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&chunk[..n]);
            }
            seen
        });
        (addr, thread)
    }

    #[test]
    fn a_missing_or_empty_lock_is_not_running() {
        let temp = Temp::new();
        let refusal = discover(&temp.0).unwrap_err();
        assert!(
            matches!(&refusal, Refusal::NotRunning(r) if r.starts_with("LOCK: ")),
            "{refusal:?}"
        );
        for text in ["pid=1\n", "pid=1 instance=0123 unix=/s tcp=none\n", "pid=1 tcp=127.0.0.1:1\n"]
        {
            std::fs::write(temp.0.join("LOCK"), text).unwrap();
            let refusal = discover(&temp.0).unwrap_err();
            assert_eq!(refusal, Refusal::NotRunning("LOCK names no server".into()), "{text}");
        }
    }

    #[test]
    fn only_127_0_0_1_and_ipv6_loopback_are_trusted() {
        let temp = Temp::new();
        for addr in ["10.0.0.1:1", "127.0.0.2:1", "0.0.0.0:1", "[::ffff:127.0.0.1]:1", "x:1"] {
            lock(&temp.0, addr);
            let refusal = discover(&temp.0).unwrap_err();
            assert!(matches!(refusal, Refusal::Untrusted(_)), "{addr}: {refusal:?}");
        }
    }

    #[test]
    fn a_closed_port_is_not_running() {
        let temp = Temp::new();
        let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        lock(&temp.0, &addr.to_string());
        let refusal = discover(&temp.0).unwrap_err();
        assert!(matches!(refusal, Refusal::NotRunning(_)), "{refusal:?}");
    }

    #[test]
    fn only_a_server_with_the_proof_is_trusted() {
        let temp = Temp::new();
        for answer in [Answer::WrongProof, Answer::NoProof, Answer::WrongInstance, Answer::Relayed]
        {
            let (addr, thread) = fake(answer);
            lock(&temp.0, &addr);
            let refusal = discover(&temp.0).unwrap_err();
            assert!(matches!(refusal, Refusal::Untrusted(_)), "{addr}: {refusal:?}");
            let seen = String::from_utf8(thread.join().unwrap()).unwrap();
            assert!(seen.starts_with("GET /healthz HTTP/1.1\r\n"), "{seen}");
            assert!(!seen.to_ascii_lowercase().contains("authorization"), "{seen}");
            assert!(!seen.contains(token().expose()), "{seen}");
        }
        for answer in [Answer::Proof, Answer::Many] {
            let (addr, thread) = fake(answer);
            lock(&temp.0, &addr);
            let stream = discover(&temp.0);
            assert!(stream.is_ok(), "{:?}", stream.err());
            drop(stream);
            let seen = String::from_utf8(thread.join().unwrap()).unwrap();
            assert!(!seen.to_ascii_lowercase().contains("authorization"), "{seen}");
        }
    }

    /// [`super::MIN_TENANT_JSON`] is the smallest tenant entry the config parser takes.
    #[test]
    fn the_probe_cap_covers_the_most_tenants_a_config_can_hold() {
        let entry = serde_json::json!({"name": "a", "token": "t".repeat(32)}).to_string();
        assert_eq!(entry.len() as u64 + 1, super::MIN_TENANT_JSON);
        let most = crate::config::MAX_FILE_BYTES / super::MIN_TENANT_JSON;
        let proofs: Vec<String> = (0..most).map(|_| "0".repeat(64)).collect();
        let body =
            serde_json::json!({"status": "ok", "instance": "0".repeat(32), "proofs": proofs});
        assert!(body.to_string().len() as u64 <= super::PROOF_BYTES);
    }
}
