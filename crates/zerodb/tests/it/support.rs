use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zerodb::auth::Auth;
use zerodb::config::{Tenant, Token};
use zerodb::serve::{self, Listeners, ServeConfig, Server};
use zerodb_core::{Engine, EngineOptions};
use zerodb_tape::{SimDisk, SystemClock, TapeOptions};

pub const TOKEN_A: &str = "token-a-0123456789abcdefghijklmnopqrstuv";
pub const TOKEN_B: &str = "token-b-0123456789abcdefghijklmnopqrstuv";

/// A private directory with a short path. Socket paths, staging included, must fit
/// `sun_path` (104 bytes on macOS), so it lives under `target/s` by default.
/// Set `ZERODB_TEST_DIR` to a short directory when the checkout path is too deep.
#[derive(Debug)]
pub struct Dir(pub PathBuf);

impl Dir {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::var_os("ZERODB_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("s"));
        std::fs::create_dir_all(&root).unwrap();
        let name = format!("{:x}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = root.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let staging = path.join("d/.zs.01234567/s");
        assert!(
            staging.as_os_str().len() < zerodb::listen::SUN_PATH_BYTES,
            "{} is too deep for socket tests, set ZERODB_TEST_DIR to a short directory",
            path.display()
        );
        Self(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn sim_engine(disk: &SimDisk, floor: Option<u64>, max_sessions: usize) -> Arc<Engine> {
    let tape =
        TapeOptions { segment_size: 1 << 20, free_space_floor: floor, ..TapeOptions::default() };
    let opts = EngineOptions {
        tape,
        checkpoint_interval: Duration::from_secs(3600),
        checkpoint_bytes: u64::MAX,
        max_sessions,
        ..EngineOptions::default()
    };
    let disk = Arc::new(disk.clone());
    Arc::new(Engine::open_with(disk, Arc::new(SystemClock), "db", opts).unwrap())
}

/// Where a request goes.
#[derive(Clone, Debug)]
pub enum Via {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl Via {
    pub fn name(&self) -> &'static str {
        match self {
            Via::Tcp(_) => "tcp",
            Via::Unix(_) => "unix",
        }
    }

    fn connect(&self) -> Box<dyn zerodb::http::Stream> {
        match self {
            Via::Tcp(addr) => Box::new(TcpStream::connect(addr).unwrap()),
            Via::Unix(path) => Box::new(UnixStream::connect(path).unwrap()),
        }
    }

    /// Writes raw bytes and reads until the server closes the connection.
    pub fn raw(&self, bytes: &[u8]) -> String {
        let mut stream = self.connect();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(bytes).unwrap();
        let mut out = Vec::new();
        stream.read_to_end(&mut out).unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    /// One request with the given `Host` and token. Returns the status and JSON body.
    pub fn call(
        &self,
        host: Option<&str>,
        token: Option<&str>,
        method: &str,
        target: &str,
        body: Option<&Value>,
    ) -> (u16, Value) {
        let auth = token.map(|token| format!("Bearer {token}"));
        let mut headers = vec![];
        headers.extend(host.map(|host| ("Host", host)));
        headers.extend(auth.as_deref().map(|auth| ("Authorization", auth)));
        let body = body.map(Value::to_string).unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(30);
        let response = zerodb::http::exchange(
            &mut *self.connect(),
            method,
            target,
            &headers,
            body.as_bytes(),
            deadline,
            64 << 20,
        )
        .unwrap();
        (response.status, serde_json::from_slice(&response.body).unwrap())
    }

    /// A request from tenant A with a loopback `Host`.
    pub fn a(&self, method: &str, target: &str, body: Option<&Value>) -> (u16, Value) {
        self.call(Some("127.0.0.1"), Some(TOKEN_A), method, target, body)
    }
}

/// An append body with one user turn.
pub fn append_body(session: &str, key: &str, content: &str) -> Value {
    json!({"agent": "a", "session": session, "idem_key": key,
           "turns": [{"role": "user", "content": content, "meta": {"k": 1}}]})
}

/// A server on TCP port 0 and a Unix socket `a` in `dir`, serving tenants `a` and `b`.
pub struct Running {
    pub server: Server,
    pub vias: [Via; 2],
    pub instance: String,
}

pub fn serve(engine: Arc<Engine>, dir: &Dir, max_connections: usize) -> Running {
    let socket = dir.0.join("a");
    let instance = zerodb::random_hex(16).unwrap();
    let tenants = [("a", TOKEN_A), ("b", TOKEN_B)].map(|(name, token)| Tenant {
        name: name.into(),
        token: Token::new(token.into()).unwrap(),
    });
    let listeners = Listeners {
        tcp: Some(zerodb::listen::bind_tcp("127.0.0.1:0".parse().unwrap()).unwrap()),
        unix: Some((
            zerodb::listen::install_unix(&socket, None, &instance).unwrap(),
            socket.clone(),
        )),
    };
    let config = ServeConfig {
        auth: Auth::new(&tenants),
        instance: instance.clone(),
        exit_on_poison: false,
        max_connections,
    };
    let server = serve::run(engine, listeners, config).unwrap();
    let vias = [Via::Tcp(server.tcp_addr().unwrap()), Via::Unix(socket)];
    Running { server, vias, instance }
}

/// A spawned `zerodb` process. Its output is collected. Drop kills it.
pub struct Process {
    pub child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl Process {
    /// Runs `zerodb` with `args` in directory `cwd`.
    pub fn spawn(cwd: &Path, args: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zerodb"));
        Self::start(command.args(args).current_dir(cwd).stdin(Stdio::null()))
    }

    /// Runs a command with stdout and stderr collected.
    pub fn start(command: &mut Command) -> Self {
        let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let collect = |stream: Box<dyn Read + Send>| {
            let sink = Arc::new(Mutex::new(String::new()));
            let lines = BufReader::new(stream).lines();
            let writer = sink.clone();
            std::thread::spawn(move || {
                for line in lines.map_while(Result::ok) {
                    let mut sink = writer.lock().unwrap();
                    sink.push_str(&line);
                    sink.push('\n');
                }
            });
            sink
        };
        let stdout = collect(Box::new(child.stdout.take().unwrap()));
        let stderr = collect(Box::new(child.stderr.take().unwrap()));
        Self { child, stdout, stderr }
    }

    /// Complete lines printed to stdout so far.
    pub fn stdout(&self) -> String {
        self.stdout.lock().unwrap().clone()
    }

    /// Complete lines printed to stderr so far.
    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Everything the process printed so far, stdout then stderr.
    pub fn output(&self) -> String {
        self.stdout() + &self.stderr()
    }

    /// Waits for the startup line and returns `instance` and the TCP address, if any.
    pub fn ready(&mut self) -> (String, Option<SocketAddr>) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let output = self.output();
            if let Some(line) = output.lines().find(|line| line.starts_with("zerodb: serving ")) {
                let field = |name: &str| {
                    let start = line.find(name).unwrap() + name.len();
                    line[start..].split(' ').next().unwrap().to_owned()
                };
                return (field("instance="), field("tcp=").parse().ok());
            }
            let exited = self.child.try_wait().unwrap();
            assert!(exited.is_none() && Instant::now() < deadline, "not serving: {output}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits up to 30 s for exit. Returns the status with everything printed.
    pub fn wait(&mut self) -> (ExitStatus, String) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                // The reader threads see end of file once the process is gone.
                std::thread::sleep(Duration::from_millis(50));
                return (status, self.output());
            }
            assert!(Instant::now() < deadline, "did not exit: {}", self.output());
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
