//! `zerodb serve`: startup order, accept loops, one thread per connection, and
//! `--exit-on-poison`.
//!
//! Memory: at most [`MAX_CONNECTIONS`] connections. A request body grows only as its
//! bytes arrive, up to the 8 MiB cap, and the raw body is dropped once parsed into
//! turns. So body memory stays under 256 x 2 x 8 MiB = 4 GiB in the worst case, where
//! every connection sends a full 8 MiB body at once.

use crate::api::{self, Route};
use crate::auth::Auth;
use crate::config::{Config, Listen};
use crate::http::{Limits, Response, ServerConn, Stream};
use crate::listen;
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use zerodb_core::{Engine, EngineError, EngineOptions, ErrorKind, Health};
use zerodb_tape::{Error, Tape};

/// Default cap on open connections. Past it a new connection gets 503 `busy`.
pub const MAX_CONNECTIONS: usize = 256;
/// Exit code when the data directory lock is held by another process.
pub const EXIT_LOCKED: u8 = 75;
/// Exit code after `--exit-on-poison` stops a poisoned server.
pub const EXIT_POISONED: u8 = 70;

/// Bound listeners. Either may be absent.
#[derive(Debug, Default)]
pub struct Listeners {
    /// Loopback TCP.
    pub tcp: Option<TcpListener>,
    /// The Unix socket and the path it is reachable at. The path wakes the accept
    /// loop on shutdown and is removed on a poison exit.
    pub unix: Option<(UnixListener, PathBuf)>,
}

/// What [`run`] needs besides the engine and the listeners.
#[derive(Debug)]
pub struct ServeConfig {
    /// Tenant tokens.
    pub auth: Auth,
    /// This start's instance id, also in `LOCK` and `/healthz`.
    pub instance: String,
    /// Stop and remove the socket path when the engine is poisoned.
    pub exit_on_poison: bool,
    /// Most open connections, [`MAX_CONNECTIONS`] for `zerodb serve`.
    pub max_connections: usize,
}

#[derive(Default)]
struct Conns {
    next: u64,
    open: HashMap<u64, Box<dyn Stream + Send>>,
    in_flight: usize,
    exited: bool,
}

#[derive(Default)]
struct Tracker {
    conns: Mutex<Conns>,
    changed: Condvar,
}

impl Tracker {
    fn lock(&self) -> MutexGuard<'_, Conns> {
        self.conns.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_until(&self, done: impl Fn(&Conns) -> bool) {
        let conns = self.lock();
        let _conns = self
            .changed
            .wait_while(conns, |conns| !done(conns))
            .unwrap_or_else(PoisonError::into_inner);
    }

    fn update(&self, change: impl FnOnce(&mut Conns)) {
        change(&mut self.lock());
        self.changed.notify_all();
    }
}

struct Shared {
    engine: Arc<Engine>,
    config: ServeConfig,
    tcp: Option<SocketAddr>,
    unix: Option<PathBuf>,
    stopping: AtomicBool,
    tracker: Arc<Tracker>,
}

impl Shared {
    /// Stops the accept loops: they see the flag after a wake-up connection.
    /// Returns false when they were already stopping.
    fn stop(&self) -> bool {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return false;
        }
        if let Some(addr) = self.tcp {
            let _ = TcpStream::connect(addr);
        }
        if let Some(path) = &self.unix {
            let _ = UnixStream::connect(path);
        }
        true
    }

    /// With `--exit-on-poison`, once the engine is poisoned: stop accepting, wait for
    /// responses in flight, remove the socket path while the lock is still held.
    fn exit_on_poison(&self) {
        if !self.config.exit_on_poison || self.engine.health() != Health::Poisoned || !self.stop() {
            return;
        }
        self.tracker.wait_until(|conns| conns.in_flight == 0);
        if let Some(path) = &self.unix {
            let _ = std::fs::remove_file(path);
        }
        self.tracker.update(|conns| conns.exited = true);
    }

    fn connection<S: Stream>(&self, stream: S, tcp: bool) {
        let mut conn = ServerConn::new(stream, Limits::default());
        let check = |head: &crate::http::Head| {
            let started = Instant::now();
            api::check(&self.config.auth, head, tcp)
                .map(|(route, tenant)| (route, tenant, started))
                .inspect_err(|response| log(&head.method, head.path(), response, "-", started))
        };
        while let Some((request, (route, tenant, started))) = conn.next_request(check) {
            // Counted once the body is in. A body still arriving at a poison exit gets a
            // reset: the client sees a transport error and retries with the same key.
            self.tracker.update(|conns| conns.in_flight += 1);
            let (method, path) = (request.head.method.clone(), request.head.path().to_owned());
            let tenant = tenant.unwrap_or_default();
            let response = match route {
                Route::Health => api::health(&self.engine, &self.config.instance),
                Route::Append => api::append(&self.engine, &tenant, request.body),
                Route::Window => api::window(&self.engine, &tenant, &request.head.target),
            };
            let _ = conn.respond(&response);
            log(&method, &path, &response, &tenant, started);
            self.tracker.update(|conns| conns.in_flight -= 1);
            if route == Route::Append {
                self.exit_on_poison();
            }
            if self.stopping.load(Ordering::Acquire) {
                break;
            }
        }
    }
}

/// One line per request. Never a header value, a body or a query string.
fn log(method: &str, path: &str, response: &Response, tenant: &str, started: Instant) {
    let tenant = if tenant.is_empty() { "-" } else { tenant };
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    eprintln!("zerodb: {method} {path} {} tenant={tenant} {ms:.3}ms", response.status);
}

/// A running server.
pub struct Server {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("tcp", &self.shared.tcp).finish_non_exhaustive()
    }
}

impl Server {
    /// The bound TCP address, if any.
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        self.shared.tcp
    }

    /// Blocks until `--exit-on-poison` has stopped the server: accepting stopped,
    /// responses in flight were written, and the socket path is gone. Without the flag
    /// it never returns.
    pub fn wait(&self) {
        self.shared.tracker.wait_until(|conns| conns.exited);
    }

    /// Stops accepting, closes every connection and joins every thread. Afterwards
    /// the server holds no reference to the engine.
    pub fn shutdown(self) {
        self.shared.stop();
        for thread in self.threads {
            let _ = thread.join();
        }
        self.shared.tracker.update(|conns| {
            for stream in conns.open.values() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        self.shared.tracker.wait_until(|conns| conns.open.is_empty());
    }
}

/// Serves an opened engine on bound listeners. Tests pass an engine on `SimDisk`.
pub fn run(engine: Arc<Engine>, listeners: Listeners, config: ServeConfig) -> io::Result<Server> {
    let tcp = listeners.tcp.as_ref().map(TcpListener::local_addr).transpose()?;
    let (unix, unix_path) = listeners.unix.unzip();
    let shared = Arc::new(Shared {
        engine,
        tcp,
        unix: unix_path,
        stopping: AtomicBool::new(false),
        tracker: Arc::default(),
        config,
    });
    let mut threads = Vec::new();
    if let Some(listener) = listeners.tcp {
        let accept = move || listener.accept().map(|(stream, _)| stream);
        threads.push(spawn_accept(&shared, accept, TcpStream::try_clone, true)?);
    }
    if let Some(listener) = unix {
        let accept = move || listener.accept().map(|(stream, _)| stream);
        threads.push(spawn_accept(&shared, accept, UnixStream::try_clone, false)?);
    }
    if shared.config.exit_on_poison {
        let shared = shared.clone();
        let tick = move || {
            while !shared.stopping.load(Ordering::Acquire) {
                thread::sleep(Duration::from_secs(1));
                shared.exit_on_poison();
            }
        };
        threads.push(thread::Builder::new().name("zerodb-poison".into()).spawn(tick)?);
    }
    Ok(Server { shared, threads })
}

fn spawn_accept<S: Stream + Send + 'static>(
    shared: &Arc<Shared>,
    accept: impl Fn() -> io::Result<S> + Send + 'static,
    clone: fn(&S) -> io::Result<S>,
    tcp: bool,
) -> io::Result<JoinHandle<()>> {
    let shared = shared.clone();
    thread::Builder::new().name("zerodb-accept".into()).spawn(move || loop {
        let accepted = accept();
        if shared.stopping.load(Ordering::Acquire) {
            return;
        }
        let Ok(stream) = accepted else {
            // Such as running out of file descriptors. Back off instead of spinning.
            thread::sleep(Duration::from_millis(10));
            continue;
        };
        let Ok(closer) = clone(&stream) else { continue };
        let mut conns = shared.tracker.lock();
        if conns.open.len() >= shared.config.max_connections {
            drop(conns);
            busy(stream);
            continue;
        }
        let id = conns.next;
        conns.next += 1;
        conns.open.insert(id, Box::new(closer));
        drop(conns);
        let worker = shared.clone();
        let tracker = shared.tracker.clone();
        let spawned = thread::Builder::new().name("zerodb-conn".into()).spawn(move || {
            worker.connection(stream, tcp);
            // Drop the engine reference before the connection counts as closed.
            drop(worker);
            tracker.update(|conns| drop(conns.open.remove(&id)));
        });
        if spawned.is_err() {
            shared.tracker.update(|conns| drop(conns.open.remove(&id)));
        }
    })
}

fn busy<S: Stream>(mut stream: S) {
    let body = r#"{"outcome":"not_committed","error":"busy"}"#;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = write!(
        stream,
        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.shutdown(Shutdown::Both);
}

/// `zerodb serve`, after flags have overridden the loaded config. Returns the exit code.
///
/// Order: TCP, then the engine and its lock, then the Unix socket. So two racing
/// starts can only collide on the lock, and only the lock holder touches the socket.
pub fn start(config: Config, exit_on_poison: bool) -> u8 {
    match try_start(config, exit_on_poison) {
        Ok(code) => code,
        Err((code, message)) => {
            eprintln!("zerodb: {message}");
            code
        }
    }
}

fn try_start(config: Config, exit_on_poison: bool) -> Result<u8, (u8, String)> {
    let fail = |message: String| (1, message);
    let data_dir = config.data_dir.ok_or_else(|| fail("set data_dir or --data-dir".into()))?;
    let data_dir = std::path::absolute(&data_dir).map_err(|error| fail(error.to_string()))?;
    let socket = config.unix_socket.unwrap_or_else(|| data_dir.join(listen::SOCKET_NAME));
    let socket = std::path::absolute(&socket).map_err(|error| fail(error.to_string()))?;
    let group = config.unix_socket_group.as_deref().map(listen::resolve_group).transpose();
    let group = group.map_err(fail)?;
    let default = Listen::Tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, listen::DEFAULT_PORT)));
    let tcp = match config.listen.unwrap_or(default) {
        Listen::None => None,
        Listen::Tcp(addr) => Some(listen::bind_tcp(addr).map_err(fail)?),
    };
    let tcp_text = match &tcp {
        Some(listener) => listener.local_addr().map_err(|e| fail(e.to_string()))?.to_string(),
        None => "none".into(),
    };
    let instance = crate::random_hex(16).map_err(|error| fail(error.to_string()))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&data_dir)
        .map_err(|error| fail(format!("cannot create {}: {error}", data_dir.display())))?;
    let mut options = EngineOptions::default();
    let address = format!("instance={instance} unix={} tcp={tcp_text}", socket.display());
    options.tape.server_address = Some(address);
    let engine = open_engine(&data_dir, options).map_err(|error| match error.kind {
        ErrorKind::Storage(Error::DataDirLocked { holder }) => (
            EXIT_LOCKED,
            format!("data directory {} is locked by {}", data_dir.display(), holder.trim()),
        ),
        _ => fail(format!("cannot open {}: {error}", data_dir.display())),
    })?;
    let unix = listen::install_unix(&socket, group).map_err(fail)?;
    let listeners = Listeners { tcp, unix: Some((unix, socket.clone())) };
    let auth = Auth::new(&config.tenants);
    let max_connections = MAX_CONNECTIONS;
    let config = ServeConfig { auth, instance, exit_on_poison, max_connections };
    let server = run(Arc::new(engine), listeners, config).map_err(|e| fail(e.to_string()))?;
    eprintln!(
        "zerodb: serving instance={} tcp={tcp_text} unix={}",
        server.shared.config.instance,
        socket.display()
    );
    server.wait();
    eprintln!("zerodb: engine poisoned, exiting so a fresh start can recover");
    Ok(EXIT_POISONED)
}

#[cfg(feature = "fault-inject")]
fn open_engine(dir: &Path, options: EngineOptions) -> Result<Engine, EngineError> {
    let io = Arc::new(crate::fault::FaultIo::new(dir));
    let clock = Arc::new(zerodb_tape::SystemClock);
    let engine = Engine::open_with(io.clone(), clock, dir, options)?;
    io.arm();
    Ok(engine)
}

#[cfg(not(feature = "fault-inject"))]
fn open_engine(dir: &Path, options: EngineOptions) -> Result<Engine, EngineError> {
    Engine::open(dir, options)
}

/// `zerodb recover --truncate-at <lsn> --data-dir <dir>`. Returns the exit code.
pub fn recover(dir: &Path, lsn: u64) -> u8 {
    match Tape::recover_truncate_at(dir, lsn) {
        Ok(report) => {
            println!("{report:?}");
            0
        }
        Err(error) => {
            eprintln!("zerodb: recover {}: {}", dir.display(), error.to_string().trim_end());
            if matches!(error, Error::DataDirLocked { .. }) {
                EXIT_LOCKED
            } else {
                1
            }
        }
    }
}
