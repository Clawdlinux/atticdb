//! How `zerodb mcp` reaches a server: the operator's `--url`, a server found through
//! `<data_dir>/LOCK` on its Unix socket, or a server it starts itself.
//!
//! Discovery never uses the TCP address in `LOCK`, and never sends the token to a
//! socket that is not ours, under an owner-only parent, answering with the `LOCK`
//! instance id.

use crate::http::{self, Stream};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{self, ErrorKind};
use std::net::TcpStream;
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
    child: Option<Child>,
    next_spawn: Instant,
    backoff: Duration,
    refused: String,
}

impl Launch {
    /// `data_dir` is absolute. Without `url` it is required, and `config` lets it start a server.
    pub fn new(url: Option<Url>, data_dir: Option<PathBuf>, config: Option<PathBuf>) -> Self {
        let refused = String::new();
        Self {
            url,
            data_dir,
            config,
            child: None,
            next_spawn: Instant::now(),
            backoff: FIRST_BACKOFF,
            refused,
        }
    }

    /// Opens a connection and returns it with the `Host` header to send. Waits for a
    /// server until `deadline`. A spawned server keeps starting after that, so a later
    /// call finds it.
    pub fn connect(&mut self, deadline: Instant) -> io::Result<(Box<dyn Stream>, String)> {
        let dir = match (&self.url, &self.data_dir) {
            (Some(Url::Unix(path)), _) => return unix(path),
            (Some(Url::Tcp(authority)), _) => {
                return Ok((Box::new(TcpStream::connect(authority.as_str())?), authority.clone()))
            }
            (None, Some(dir)) => dir.clone(),
            (None, None) => return Err(io::Error::other("no --url and no data directory")),
        };
        loop {
            if let Some(Ok(Some(status))) = self.child.as_mut().map(Child::try_wait) {
                eprintln!("zerodb mcp: started server exited: {status}");
                self.child = None;
            }
            match discover(&dir) {
                Ok(stream) => {
                    (self.backoff, self.refused) = (FIRST_BACKOFF, String::new());
                    return Ok((Box::new(stream), "localhost".into()));
                }
                Err(reason) if reason != self.refused => {
                    eprintln!("zerodb mcp: no trusted server: {reason}");
                    self.refused = reason;
                }
                Err(_) => {}
            }
            // A child that exited 75 lost the lock race, but the holder may be a poisoned
            // server shutting down. So spawn again, with backoff, until one is trusted.
            if self.child.is_none() && Instant::now() >= self.next_spawn {
                if let Some(config) = self.config.clone() {
                    self.spawn(&dir, &config)?;
                }
            }
            if Instant::now() >= deadline {
                let log = dir.join("serve.log");
                let message = format!("no trusted server in time, see {}", log.display());
                return Err(io::Error::new(ErrorKind::TimedOut, message));
            }
            std::thread::sleep(POLL);
        }
    }

    /// `zerodb serve` on the Unix socket only, outliving this adapter. The token stays
    /// in the config, never on the command line or in the environment.
    fn spawn(&mut self, dir: &Path, config: &Path) -> io::Result<()> {
        crate::listen::owned_dir(dir, 0o077).map_err(io::Error::other)?;
        let (mut log, path) = (OpenOptions::new(), dir.join("serve.log"));
        let log = log.create(true).append(true).mode(0o600).open(&path)?;
        crate::config::check_private(&log, &path, "server log").map_err(io::Error::other)?;
        let child = Command::new(std::env::current_exe()?)
            .arg("serve")
            .arg("--config")
            .arg(config)
            .arg("--data-dir")
            .arg(dir)
            .args(["--listen", "none", "--exit-on-poison"])
            .env_remove(crate::mcp::TOKEN_ENV)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()?;
        eprintln!("zerodb mcp: started server pid {}", child.id());
        self.child = Some(child);
        self.next_spawn = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
        Ok(())
    }
}

fn unix(path: &Path) -> io::Result<(Box<dyn Stream>, String)> {
    Ok((Box::new(UnixStream::connect(path)?), "localhost".into()))
}

/// A connection to the socket named in `LOCK`, if it is ours, under an owner-only parent,
/// and answered `/healthz` on this connection with the instance id in `LOCK`. The token
/// then goes over the same connection. Otherwise the reason, and no token was sent.
fn discover(dir: &Path) -> Result<UnixStream, String> {
    let lock = std::fs::read_to_string(dir.join("LOCK")).map_err(|e| format!("LOCK: {e}"))?;
    // `pid=<pid> instance=<id> unix=<path> tcp=<addr>`. The path may hold spaces.
    let fields = lock.trim_end().rsplit_once(" tcp=").map(|(head, _)| head);
    let instance =
        fields.and_then(|head| head.split(' ').find_map(|field| field.strip_prefix("instance=")));
    let socket = fields.and_then(|head| head.split_once(" unix=")).map(|(_, path)| path);
    let (Some(instance), Some(socket)) = (instance, socket.map(Path::new)) else {
        return Err("LOCK names no server".into());
    };
    // Resolve symlinks first, so the checks below cover the directories actually used.
    let parent = socket.parent().map(std::fs::canonicalize).transpose();
    let parent = parent.map_err(|error| format!("socket {}: {error}", socket.display()))?;
    let socket = parent.unwrap_or_default().join(socket.file_name().unwrap_or_default());
    let euid = rustix::process::geteuid().as_raw();
    let meta = std::fs::symlink_metadata(&socket);
    let meta = meta.map_err(|error| format!("socket {}: {error}", socket.display()))?;
    if !meta.file_type().is_socket() || meta.uid() != euid {
        return Err(format!(
            "{} must be a socket owned by uid {euid}, found owner uid {}",
            socket.display(),
            meta.uid()
        ));
    }
    let mut dirs = socket.ancestors().skip(1);
    crate::listen::owned_dir(dirs.next().unwrap_or(Path::new("/")), 0o022)?;
    // OpenSSH StrictModes: anyone who can rename a directory above the socket can swap it.
    for dir in dirs {
        let meta = std::fs::metadata(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let writable = meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0;
        if (meta.uid() != 0 && meta.uid() != euid) || writable {
            return Err(format!(
                "{} must be owned by root or uid {euid} and not writable by group or others \
                 unless sticky, found mode {:04o} owned by uid {}",
                dir.display(),
                meta.mode() & 0o7777,
                meta.uid()
            ));
        }
    }
    let fail = |error: io::Error| format!("{}: {error}", socket.display());
    let mut stream = UnixStream::connect(&socket).map_err(fail)?;
    let headers = [("Host", "localhost"), ("Connection", "keep-alive")];
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let response = http::exchange(&mut stream, "GET", "/healthz", &headers, &[], deadline, 4096);
    let response = response.map_err(fail)?;
    let health: Value = serde_json::from_slice(&response.body).unwrap_or_default();
    if matches!(response.status, 200 | 503) && health["instance"] == instance {
        Ok(stream)
    } else {
        Err(format!("{} is not instance {instance} from LOCK", socket.display()))
    }
}
