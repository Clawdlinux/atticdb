//! `zerodb serve`: startup order, accept loops, one thread per connection, and
//! `--exit-on-poison`.
//!
//! Memory: at most [`MAX_CONNECTIONS`] connections. A request body grows only as its
//! bytes arrive, up to the 8 MiB cap, and the raw body is dropped once parsed into
//! turns, which are no larger. At most 2 JSON trees exist at once, request parses and
//! window builds together, and a tree can be about 16x its body. A window reads at most
//! 8 MiB of turns; its body is at most 6x that, since JSON escapes a byte to at most 6.
//! Window bodies being written total at most 256 MiB, and at most 2 more wait for that
//! budget, up to 5 s before 503 `busy`. So request and window memory stays under about
//! 256 x 8 MiB = 2 GiB of raw bodies or turns, plus 2 trees of about 16 x 8 MiB, plus
//! 256 MiB + 2 x 49 MiB of window bodies, in the worst case.

use crate::api::{self, Route};
use crate::auth::Auth;
use crate::config::{Config, Listen};
use crate::http::{self, Limits, Response, ServerConn, Stream};
use crate::listen::{self, Installed};
use rustix::event::{PollFd, PollFlags, Timespec};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
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
    /// The Unix socket and the path it is installed at. The path is removed on a
    /// poison exit if it is still ours.
    pub unix: Option<(UnixListener, Installed)>,
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
    /// One stderr line per request. Off by default: every connection thread shares the
    /// lock. 5xx answers are always logged.
    pub log_requests: bool,
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
    unix: Option<Installed>,
    stopping: AtomicBool,
    tracker: Arc<Tracker>,
}

impl Shared {
    /// Stops the accept loops, which check the flag at least every 50 ms. Returns false
    /// when they were already stopping.
    fn stop(&self) -> bool {
        !self.stopping.swap(true, Ordering::AcqRel)
    }

    /// With `--exit-on-poison`, once the engine is poisoned: stop accepting, wait for
    /// responses in flight, remove the socket path while the lock is still held.
    fn exit_on_poison(&self) {
        if !self.config.exit_on_poison || self.engine.health() != Health::Poisoned || !self.stop() {
            return;
        }
        self.tracker.wait_until(|conns| conns.in_flight == 0);
        if let Some(unix) = &self.unix {
            let _ = unix.remove();
        }
        self.tracker.update(|conns| conns.exited = true);
    }

    fn connection<S: Stream>(&self, stream: S, tcp: bool) {
        let mut conn = ServerConn::new(stream, Limits::default());
        let check = |head: &crate::http::Head| {
            let started = Instant::now();
            api::check(&self.config.auth, head, tcp)
                .map(|(route, tenant)| (route, tenant, started))
                .inspect_err(|response| {
                    self.log(&head.method, head.path(), response, "-", started);
                })
        };
        while let Some((request, (route, tenant, started))) = conn.next_request(check) {
            // Counted once the body is in. A body still arriving at a poison exit gets a
            // reset: the client sees a transport error and retries with the same key.
            self.tracker.update(|conns| conns.in_flight += 1);
            let (method, path) = (request.head.method.clone(), request.head.path().to_owned());
            let tenant = tenant.unwrap_or_default();
            let response = match route {
                Route::Health => api::health(&self.engine, &self.config.instance),
                Route::Stats => api::stats(&self.engine, &self.config.instance),
                Route::Metrics => api::metrics(&self.engine),
                Route::Append => api::append(&self.engine, &tenant, request.body),
                Route::Window => api::window(&self.engine, &tenant, &request.head.target),
                Route::FactPut | Route::FactDelete => {
                    api::fact_write(&self.engine, &tenant, request.body, route == Route::FactDelete)
                }
                Route::FactGet | Route::FactHistory | Route::FactList => {
                    api::fact_read(&self.engine, &tenant, route, &request.head.target)
                }
                Route::Search => api::search(&self.engine, &tenant, &request.head.target),
                Route::Sessions => api::sessions(&self.engine, &tenant, &request.head.target),
                Route::Records => api::records(&self.engine, &tenant, &request.head.target),
            };
            let _ = conn.respond(&response);
            self.log(&method, &path, &response, &tenant, started);
            self.tracker.update(|conns| conns.in_flight -= 1);
            self.exit_on_poison();
            if self.stopping.load(Ordering::Acquire) {
                break;
            }
        }
    }
}

impl Shared {
    /// One line per logged request. Never a header value, a body or a query string.
    fn log(&self, method: &str, path: &str, response: &Response, tenant: &str, started: Instant) {
        if self.config.log_requests || response.status >= 500 {
            log(method, path, response, tenant, started);
        }
    }
}

fn log(method: &str, path: &str, response: &Response, tenant: &str, started: Instant) {
    let tenant = if tenant.is_empty() { "-" } else { tenant };
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    // Not `eprintln!`: it panics when stderr fails, such as on a full disk under serve.log.
    let status = response.status;
    let _ = writeln!(io::stderr(), "zerodb: {method} {path} {status} tenant={tenant} {ms:.3}ms");
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
    // Accepted streams can inherit nonblocking mode from the listener, as on macOS.
    if let Some(listener) = listeners.tcp {
        listener.set_nonblocking(true)?;
        let accept = move || {
            let (stream, _) = ready(&listener)?.accept()?;
            // A response is written as a head and a body, so do not hold the body back.
            stream.set_nodelay(true)?;
            stream.set_nonblocking(false).map(|()| stream)
        };
        threads.push(spawn_accept(&shared, accept, TcpStream::try_clone, true)?);
    }
    if let Some(listener) = unix {
        listener.set_nonblocking(true)?;
        let accept = move || {
            let (stream, _) = ready(&listener)?.accept()?;
            stream.set_nonblocking(false).map(|()| stream)
        };
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

/// Waits up to 50 ms for a connection. So the accept loop sees `stopping` without a
/// wake-up connection, which cannot reach a socket whose path was unlinked or replaced.
fn ready<L: AsFd>(listener: &L) -> io::Result<&L> {
    let mut fds = [PollFd::new(listener, PollFlags::IN)];
    match rustix::event::poll(&mut fds, Some(&Timespec { tv_sec: 0, tv_nsec: 50_000_000 }))? {
        0 => Err(io::ErrorKind::WouldBlock.into()),
        _ => Ok(listener),
    }
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
        let Ok(stream) = accepted.inspect_err(|error| {
            // Such as running out of file descriptors. Back off instead of spinning.
            if error.kind() != io::ErrorKind::WouldBlock {
                thread::sleep(Duration::from_millis(10));
            }
        }) else {
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

/// Answers 503 on accept without reading, and closes without lingering.
fn busy<S: Stream>(mut stream: S) {
    let deadline = Instant::now() + Duration::from_secs(1);
    let _ = http::write_response(&mut stream, &api::busy(), true, deadline);
    let _ = stream.shutdown(Shutdown::Both);
}

/// `zerodb serve`, after flags have overridden the loaded config. Returns the exit code.
///
/// Order: TCP, then the engine and its lock, then the Unix socket. So two racing
/// starts can only collide on the lock, and only the lock holder touches the socket.
pub fn start(config: Config, exit_on_poison: bool, log_requests: bool) -> u8 {
    match try_start(config, exit_on_poison, log_requests) {
        Ok(code) => code,
        Err((code, message)) => {
            let _ = writeln!(io::stderr(), "zerodb: {message}");
            code
        }
    }
}

fn try_start(config: Config, exit_on_poison: bool, log_requests: bool) -> Result<u8, (u8, String)> {
    let fail = |message: String| (1, message);
    let data_dir = config.data_dir.ok_or_else(|| fail("set data_dir or --data-dir".into()))?;
    let data_dir = std::path::absolute(&data_dir).map_err(|error| fail(error.to_string()))?;
    let socket = config.unix_socket.unwrap_or_else(|| data_dir.join(listen::SOCKET_NAME));
    let socket = std::path::absolute(&socket).map_err(|error| fail(error.to_string()))?;
    let group = config.unix_socket_group.as_deref().map(listen::resolve_group).transpose();
    let group = group.map_err(fail)?;
    let locked = |holder: String| {
        let dir = data_dir.display();
        (EXIT_LOCKED, format!("data directory {dir} is locked by {}", holder.trim()))
    };
    let default = Listen::Tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, listen::DEFAULT_PORT)));
    let tcp = match config.listen.unwrap_or(default) {
        Listen::None => None,
        // A second server on a fixed port fails here, before the lock. Say who holds it.
        Listen::Tcp(addr) => Some(listen::bind_tcp(addr).map_err(|error| {
            match lock_holder(&data_dir).filter(|_| error.kind() == io::ErrorKind::AddrInUse) {
                Some(holder) => locked(holder),
                None => fail(error.to_string()),
            }
        })?),
    };
    let tcp_text = match &tcp {
        Some(listener) => listener.local_addr().map_err(|e| fail(e.to_string()))?.to_string(),
        None => "none".into(),
    };
    let instance = crate::random_hex(16).map_err(|error| fail(error.to_string()))?;
    listen::owned_dir(&data_dir, 0o077).map_err(fail)?;
    let mut options = EngineOptions::default();
    let address = format!("instance={instance} unix={} tcp={tcp_text}", socket.display());
    options.tape.server_address = Some(address);
    let engine = open_engine(&data_dir, options).map_err(|error| match error.kind {
        ErrorKind::Storage(Error::DataDirLocked { holder }) => locked(holder),
        _ => fail(format!("cannot open {}: {error}", data_dir.display())),
    })?;
    let unix = listen::install_unix(&socket, group, &instance).map_err(fail)?;
    let listeners = Listeners { tcp, unix: Some(unix) };
    let auth = Auth::new(&config.tenants);
    let max_connections = MAX_CONNECTIONS;
    let config = ServeConfig { auth, instance, exit_on_poison, max_connections, log_requests };
    let server = run(Arc::new(engine), listeners, config).map_err(|e| fail(e.to_string()))?;
    // One write, so a reader never sees half of the line.
    let (instance, socket) = (&server.shared.config.instance, socket.display());
    let line = format!("zerodb: serving instance={instance} tcp={tcp_text} unix={socket}\n");
    let _ = io::stderr().write_all(line.as_bytes());
    server.wait();
    let _ = writeln!(io::stderr(), "zerodb: engine poisoned, exiting so a fresh start can recover");
    Ok(EXIT_POISONED)
}

/// The `LOCK` contents when another process holds the data directory lock.
fn lock_holder(dir: &Path) -> Option<String> {
    let mut options = std::fs::OpenOptions::new();
    let mut file = crate::config::open_regular(options.read(true), &dir.join("LOCK")).ok()?;
    let held = matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock));
    let mut holder = String::new();
    (held && file.read_to_string(&mut holder).is_ok()).then_some(holder)
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
            let text = error.to_string();
            let _ =
                writeln!(io::stderr(), "zerodb: recover {}: {}", dir.display(), text.trim_end());
            if matches!(error, Error::DataDirLocked { .. }) {
                EXIT_LOCKED
            } else {
                1
            }
        }
    }
}
