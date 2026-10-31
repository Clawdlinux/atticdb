use atticdb::auth::Auth;
use atticdb::config::{Tenant, Token};
use atticdb::serve::{self, Listeners, ServeConfig, Server};
use atticdb_core::{Engine, EngineOptions};
use atticdb_tape::{SimDisk, SystemClock, TapeOptions};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Every entry name in `dir` with its bytes, or `None` when it cannot be read as a file.
pub fn files(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let entries = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap());
    entries.map(|e| (e.file_name().into_string().unwrap(), std::fs::read(e.path()).ok())).collect()
}

/// Every path under `dir`, with a file's bytes or a symlink's target, or `None` for a
/// directory. Symlinks aren't followed.
pub fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    tree_outside(dir, None)
}

/// [`tree`] without `skip` and what is under it, such as a server's data directory.
pub fn tree_outside(dir: &Path, skip: Option<&Path>) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let (mut all, mut todo) = (BTreeMap::new(), vec![dir.to_owned()]);
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if Some(path.as_path()) == skip {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                todo.push(path.clone());
                all.insert(path, None);
            } else if meta.is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                all.insert(path, Some(target.into_os_string().into_encoded_bytes()));
            } else {
                all.insert(path.clone(), Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    all
}

/// A program `name` in the existing directory `bin` that appends its args to `bin/<name>.argv`
/// and exits 1, for a harness that must be found on `PATH` but never run.
pub fn fake_program(bin: &Path, name: &str) {
    let program = bin.join(name);
    let script = "#!/bin/sh\necho \"$*\" >> \"$0.argv\"\nexit 1\n";
    std::fs::write(&program, script).unwrap();
    chmod(&program, 0o755);
}

/// Variables that point AtticDB at a home or at harness files, the real `HOME` included. No
/// spawn inherits them, so a developer's own exports can't lead a test to a real home. A test
/// that needs one sets it.
const HOME_VARS: [&str; 7] = [
    "HOME",
    "ATTICDB_HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "COPILOT_HOME",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
];

/// A command for `program`, the `atticdb` binary or anything that runs it, without
/// [`HOME_VARS`]. Every spawn of AtticDB in these tests starts here.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for var in HOME_VARS {
        command.env_remove(var);
    }
    command
}

/// A fake `claude` in the new directory `bin`, for a `PATH` of just `bin`. It appends its
/// args as a JSON line to `bin/argv`, which it returns. `mcp add` and `mcp remove -s user
/// <name>` edit `.claude.json` in `$CLAUDE_CONFIG_DIR`, else in `$HOME`, as Claude Code does.
/// `mcp add` parses as the real CLI does: `-e` takes `K=V` values until `--` or the next
/// option, so `-e K=V <name>` fails. `mcp get` answers from the file. With
/// `FAKE_CLAUDE=slow`, it starts a python child that writes `$FAKE_CLAUDE_MARKER` after 12 s,
/// then sleeps 60 s. With `FAKE_CLAUDE=add-fails`, `mcp add` exits 1.
pub fn fake_claude(bin: &Path) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let python = std::env::split_paths(&path).map(|dir| dir.join("python3")).find(|p| p.is_file());
    let python = python.unwrap_or_else(|| panic!("the fake claude needs python3 on PATH"));
    let script = format!(
        r#"#!{}
import json, os, subprocess, sys, time
args = sys.argv[1:]
with open(os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "argv"), "a") as log:
    log.write(json.dumps(args) + "\n")
mode = os.environ.get("FAKE_CLAUDE", "")
if mode == "slow":
    marker = os.environ["FAKE_CLAUDE_MARKER"]
    # Python itself, as PATH holds only this fake's directory, with no sleep or touch.
    later = "import sys, time; time.sleep(12); open(sys.argv[1], 'w').close()"
    subprocess.Popen([sys.executable, "-c", later, marker])
    time.sleep(60)
path = os.path.join(os.environ.get("CLAUDE_CONFIG_DIR") or os.environ["HOME"], ".claude.json")
try:
    with open(path) as f:
        root = json.load(f)
except FileNotFoundError:
    root = {{}}
servers = root.setdefault("mcpServers", {{}})
if args[:2] == ["mcp", "get"]:
    print(json.dumps(servers.get(args[2])))
    sys.exit(0 if args[2] in servers else 1)
if args[:2] == ["mcp", "add"]:
    if mode == "add-fails":
        sys.exit("add failed")
    rest, env, scope, words = args[2:], {{}}, None, []
    while rest:
        arg = rest.pop(0)
        if arg == "--":
            words += rest
            break
        if arg in ("-s", "--scope"):
            scope = rest.pop(0)
        elif arg in ("-e", "--env"):
            while rest and not rest[0].startswith("-"):
                value = rest.pop(0)
                if "=" not in value:
                    sys.exit("Invalid environment variable format: " + value)
                key, value = value.split("=", 1)
                env[key] = value
        elif arg.startswith("-"):
            sys.exit("unknown option " + arg)
        else:
            words.append(arg)
    if scope != "user" or len(words) < 2:
        sys.exit("usage: claude mcp add [options] <name> <commandOrUrl> [args...]")
    if words[0] in servers:
        sys.exit("MCP server " + words[0] + " already exists in user config")
    servers[words[0]] = {{"type": "stdio", "command": words[1], "args": words[2:], "env": env}}
elif args[:4] == ["mcp", "remove", "-s", "user"] and len(args) == 5:
    if servers.pop(args[4], None) is None:
        sys.exit("No user-scoped MCP server found with name: " + args[4])
else:
    sys.exit("unexpected args")
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w") as f:
    json.dump(root, f, indent=2)
"#,
        python.display()
    );
    std::fs::create_dir(bin).unwrap();
    let claude = bin.join("claude");
    // `fs::write` closes the file before it returns, so no write fd outlives this helper.
    std::fs::write(&claude, script).unwrap();
    chmod(&claude, 0o755);
    bin.join("argv")
}

/// A fake `codex` in the new directory `bin`, for a `PATH` of just `bin`. It appends its args
/// as a JSON line to `bin/argv`, which it returns, and its working directory as a line to
/// `bin/cwd`. It answers only `mcp get <name> [--json]`,
/// as Codex 0.147.0 does: it parses `config.toml` in `$CODEX_HOME`, else in `$HOME/.codex`,
/// with Python's `tomllib`, so a file that isn't valid TOML fails. A missing server exits 1.
/// `--json` prints the server as `{"name", "enabled", "transport": {"type": "stdio",
/// "command", "args", "env", ...}, ...}`. With `FAKE_CODEX=mismatch`, it shows another
/// command. With `FAKE_CODEX=garbage`, it prints text that isn't JSON. With
/// `FAKE_CODEX=fails`, it prints `$FAKE_CODEX_TOKEN` on stdout and stderr, as an error quoting
/// the config would, and exits 1. With `FAKE_CODEX=edits`, it appends a table to `config.toml`,
/// as Codex trusting a project would, then shows another command.
pub fn fake_codex(bin: &Path) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let python = std::env::split_paths(&path).map(|dir| dir.join("python3")).find(|p| p.is_file());
    let python = python.unwrap_or_else(|| panic!("the fake codex needs python3 on PATH"));
    // CI's runners have Python 3.12 (Ubuntu 24.04) and 3.14 (macOS 15).
    let mut check = command(&python);
    check.args(["-c", "import tomllib"]).stdout(Stdio::null()).stderr(Stdio::null());
    let has_tomllib = spawn(&mut check).wait().unwrap().success();
    assert!(has_tomllib, "the fake codex needs Python 3.11 or later, for tomllib: {python:?}");
    let script = format!(
        r#"#!{}
import json, os, sys
try:
    import tomllib
except ImportError:
    sys.exit("the fake codex needs Python 3.11 or later, for tomllib")
args = sys.argv[1:]
with open(os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "argv"), "a") as log:
    log.write(json.dumps(args) + "\n")
with open(os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "cwd"), "a") as log:
    log.write(os.getcwd() + "\n")
mode = os.environ.get("FAKE_CODEX", "")
if mode == "fails":
    line = "3 | api_key = \"" + os.environ["FAKE_CODEX_TOKEN"] + "\""
    print(line)
    sys.exit("Error loading config.toml: TOML parse error at line 3\n" + line + "\ninvalid")
if args[:2] != ["mcp", "get"]:
    sys.exit("unexpected args")
name, as_json = None, False
for arg in args[2:]:
    if arg == "--json":
        as_json = True
    elif arg.startswith("-") or name is not None:
        sys.exit("error: unexpected argument '" + arg + "' found")
    else:
        name = arg
if name is None:
    sys.exit("error: the following required arguments were not provided: <NAME>")
home = os.environ.get("CODEX_HOME") or os.path.join(os.environ["HOME"], ".codex")
if not os.path.isdir(home):
    sys.exit("Error: CODEX_HOME points to " + repr(home) + ", but that path does not exist")
try:
    with open(os.path.join(home, "config.toml"), "rb") as f:
        config = tomllib.load(f)
except FileNotFoundError:
    config = {{}}
except tomllib.TOMLDecodeError as error:
    sys.exit("Error loading config.toml: " + str(error))
server = config.get("mcp_servers", {{}}).get(name)
if server is None:
    sys.exit("Error: No MCP server named '" + name + "' found.")
modes = ("auto", "prompt", "writes", "approve")
tools = server.get("tools", {{}})
if server.get("default_tools_approval_mode", "auto") not in modes or any(
    tool.get("approval_mode", "auto") not in modes for tool in tools.values()
):
    sys.exit("Error loading config.toml: unknown approval mode")
if mode == "edits":
    with open(os.path.join(home, "config.toml"), "a") as f:
        f.write('\n[projects."/edited"]\ntrust_level = "trusted"\n')
if mode in ("mismatch", "edits"):
    server["command"] = "/elsewhere/atticdb"
if mode == "garbage" or not as_json:
    print(name + "\n  enabled: true")
    sys.exit(0)
transport = {{"type": "stdio", "command": server.get("command"), "args": server.get("args", []),
    "env": server.get("env"), "env_vars": server.get("env_vars", []), "cwd": server.get("cwd")}}
print(json.dumps({{"name": name, "enabled": server.get("enabled", True),
    "disabled_reason": None, "transport": transport, "enabled_tools": None,
    "disabled_tools": None, "startup_timeout_sec": None, "tool_timeout_sec": None}}, indent=2))
"#,
        python.display()
    );
    std::fs::create_dir(bin).unwrap();
    let codex = bin.join("codex");
    // `fs::write` closes the file before it returns, so no write fd outlives this helper.
    std::fs::write(&codex, script).unwrap();
    chmod(&codex, 0o755);
    bin.join("argv")
}

/// Spawns `command`, retrying while Linux says the program is busy. A test may run a file it
/// just wrote, such as a fake `claude` or a copied binary. If another test thread forks
/// while that file is open for writing, the child holds the write fd until it execs, and an
/// exec of the file meanwhile fails with ETXTBSY. Helpers close each file before returning,
/// so the window is short: up to 50 tries, 20 ms apart.
fn spawn(command: &mut Command) -> Child {
    for _ in 1..50 {
        match command.spawn() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(20));
            }
            spawned => return spawned.unwrap(),
        }
    }
    command.spawn().unwrap()
}

pub fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Makes a mode 0600 FIFO at `path`.
pub fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo").args(["-m", "600"]).arg(path).status();
    assert!(status.unwrap().success(), "mkfifo {}", path.display());
}

/// A Unix listener at `path` that never accepts, with its backlog full. Holds the
/// listener and the queued connections until dropped.
pub fn saturated(path: &Path) -> Vec<OwnedFd> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};
    let listener = UnixListener::bind(path).unwrap();
    rustix::net::listen(&listener, 0).unwrap();
    let mut held = vec![OwnedFd::from(listener)];
    for _ in 0..1024 {
        let socket = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
        rustix::io::ioctl_fionbio(&socket, true).unwrap();
        if rustix::net::connect(&socket, &SocketAddrUnix::new(path).unwrap()).is_err() {
            return held;
        }
        held.push(socket);
    }
    panic!("the backlog of {} never filled", path.display());
}

pub const TOKEN_A: &str = "token-a-0123456789abcdefghijklmnopqrstuv";
pub const TOKEN_B: &str = "token-b-0123456789abcdefghijklmnopqrstuv";

/// A private directory with a short path. Socket paths, staging included, must fit
/// `sun_path` (104 bytes on macOS). By default it lives under `/tmp/atticdb-test-<uid>` on
/// macOS and `/var/tmp/atticdb-test-<uid>` on Linux, whatever the checkout path.
/// `ATTICDB_TEST_DIR` is optional and may be any short directory. It is resolved to its real
/// path.
#[derive(Debug)]
pub struct Dir(pub PathBuf);

impl Dir {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Its real path, since servers build absolute paths from `getcwd` and tests compare them.
        let root = std::env::var_os("ATTICDB_TEST_DIR").map_or_else(tmp_root, |root| {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::canonicalize(root).unwrap()
        });
        let name = format!("{:x}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = root.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let staging = path.join("d/.zs.01234567/s");
        assert!(
            staging.as_os_str().len() < atticdb::listen::SUN_PATH_BYTES,
            "{} is too deep for socket tests, set ATTICDB_TEST_DIR to a short directory",
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

/// `/tmp/atticdb-test-<uid>` on macOS and `/var/tmp/atticdb-test-<uid>` on Linux, made with mode
/// 0700 if missing. Linux's `/tmp` may be RAM, and each test server can pin up to 128 MiB there.
/// macOS keeps `/tmp`: its `/var/tmp` resolves 4 bytes longer, too long for a 10-digit uid.
/// The path is real, since macOS links `/tmp` to `/private/tmp` and tests compare socket paths.
/// Anyone can add names there, so the root must be this user's private directory and not a
/// symlink.
fn tmp_root() -> PathBuf {
    let uid = rustix::process::geteuid().as_raw();
    // Minimal containers may have no `/var/tmp`, so fall back to `/tmp` there.
    let wanted = if cfg!(target_os = "macos") { "/tmp" } else { "/var/tmp" };
    let base = std::fs::canonicalize(wanted).or_else(|_| std::fs::canonicalize("/tmp"));
    let base = base.unwrap_or_else(|error| {
        panic!(
            "cannot resolve {wanted} or /tmp: {error}; set ATTICDB_TEST_DIR to a short directory"
        )
    });
    let root = base.join(format!("atticdb-test-{uid}"));
    // `owned_dir` makes the root if missing, but its check follows symlinks. So check the
    // entry itself. Once it is ours, the sticky bit on the base stops others from replacing it.
    let made = atticdb::listen::owned_dir(&root, 0o077);
    let error = match std::fs::symlink_metadata(&root) {
        Ok(meta) if meta.is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0 => return root,
        Ok(meta) if meta.is_symlink() => format!("{} is a symlink", root.display()),
        Ok(meta) => format!(
            "{} must be a directory owned by uid {uid}, mode 0700 or stricter, found mode {:04o} \
             owned by uid {}",
            root.display(),
            meta.mode() & 0o7777,
            meta.uid()
        ),
        // If `owned_dir` failed, its error says why the root is missing.
        Err(error) => made.err().unwrap_or_else(|| format!("{}: {error}", root.display())),
    };
    panic!("{error}; set ATTICDB_TEST_DIR to a short directory")
}

/// An engine on a copy of `disk` with 1 MiB segments and no timed checkpoints. `tune`
/// edits the options first.
pub fn sim_engine(disk: &SimDisk, tune: fn(&mut EngineOptions)) -> Arc<Engine> {
    let tape = TapeOptions { segment_size: 1 << 20, ..TapeOptions::default() };
    let (checkpoint_interval, checkpoint_bytes) = (Duration::from_secs(3600), u64::MAX);
    let mut opts =
        EngineOptions { tape, checkpoint_interval, checkpoint_bytes, ..EngineOptions::default() };
    tune(&mut opts);
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

    fn connect(&self) -> Box<dyn atticdb::http::Stream> {
        match self {
            Via::Tcp(addr) => Box::new(TcpStream::connect(addr).unwrap()),
            Via::Unix(path) => Box::new(UnixStream::connect(path).unwrap()),
        }
    }

    /// Writes raw bytes and reads until the server closes the connection.
    pub fn raw(&self, bytes: &[u8]) -> String {
        let mut stream = self.connect();
        // Longer than the studio's 10 s store deadline, so a 504 arrives before this. macOS
        // fails `setsockopt` with EINVAL once the peer has closed, as after a busy answer. The
        // read below then returns buffered data or EOF at once.
        if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(30))) {
            assert_eq!(error.raw_os_error(), Some(rustix::io::Errno::INVAL.raw_os_error()));
        }
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
        let response = atticdb::http::exchange(
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
    let instance = atticdb::random_hex(16).unwrap();
    let tenants = [("a", TOKEN_A), ("b", TOKEN_B)].map(|(name, token)| Tenant {
        name: name.into(),
        token: Token::new(token.into()).unwrap(),
    });
    let listeners = Listeners {
        tcp: Some(atticdb::listen::bind_tcp("127.0.0.1:0".parse().unwrap()).unwrap()),
        unix: Some(atticdb::listen::install_unix(&socket, None, &instance).unwrap()),
    };
    let config = ServeConfig {
        auth: Auth::new(&tenants),
        instance: instance.clone(),
        exit_on_poison: false,
        max_connections,
        log_requests: false,
    };
    let server = serve::run(engine, listeners, config).unwrap();
    let vias = [Via::Tcp(server.tcp_addr().unwrap()), Via::Unix(socket)];
    Running { server, vias, instance }
}

/// A spawned `atticdb` process. Its output is collected. Drop kills it.
pub struct Process {
    pub child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl Process {
    /// Runs `atticdb` with `args` in directory `cwd`.
    pub fn spawn(cwd: &Path, args: &[&str]) -> Self {
        let mut command = command(env!("CARGO_BIN_EXE_atticdb"));
        Self::start(command.args(args).current_dir(cwd).stdin(Stdio::null()))
    }

    /// Runs a command with stdout and stderr collected.
    pub fn start(command: &mut Command) -> Self {
        let mut child = spawn(command.stdout(Stdio::piped()).stderr(Stdio::piped()));
        let mut readers = Vec::new();
        let mut collect = |stream: Box<dyn Read + Send>| {
            let sink = Arc::new(Mutex::new(String::new()));
            let lines = BufReader::new(stream).lines();
            let writer = sink.clone();
            readers.push(std::thread::spawn(move || {
                for line in lines.map_while(Result::ok) {
                    let mut sink = writer.lock().unwrap();
                    sink.push_str(&line);
                    sink.push('\n');
                }
            }));
            sink
        };
        let stdout = collect(Box::new(child.stdout.take().unwrap()));
        let stderr = collect(Box::new(child.stderr.take().unwrap()));
        Self { child, stdout, stderr, readers }
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
    fn output(&self) -> String {
        self.stdout() + &self.stderr()
    }

    /// Waits for the startup line and returns `instance` and the TCP address, if any.
    pub fn ready(&mut self) -> (String, Option<SocketAddr>) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let output = self.output();
            if let Some(line) = output.lines().find(|line| line.starts_with("atticdb: serving ")) {
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
        self.wait_up_to(Duration::from_secs(30))
    }

    /// Waits up to `limit` for exit. Returns the status with everything printed.
    pub fn wait_up_to(&mut self, limit: Duration) -> (ExitStatus, String) {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                // A fixed sleep lost stderr on a busy runner. Readers end at the pipes' EOF.
                while !self.readers.iter().all(std::thread::JoinHandle::is_finished) {
                    assert!(Instant::now() < deadline, "pipes still open: {}", self.output());
                    std::thread::sleep(Duration::from_millis(5));
                }
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

/// An `atticdb mcp` process driven over stdin and stdout.
pub struct Mcp {
    pub process: Process,
    stdin: Option<ChildStdin>,
    read: usize,
    /// Requests the adapter sent, such as `roots/list`.
    asked: usize,
    pub next_id: u64,
}

impl Mcp {
    /// `token` goes in `ATTICDB_TOKEN`. Without it the flags name the token.
    pub fn start(cwd: &Path, args: &[&str], token: Option<&str>) -> Self {
        let mut command = command(env!("CARGO_BIN_EXE_atticdb"));
        command.arg("mcp").args(args).current_dir(cwd).stdin(Stdio::piped());
        command.env_remove("ATTICDB_TOKEN");
        if let Some(token) = token {
            command.env("ATTICDB_TOKEN", token);
        }
        let mut process = Process::start(&mut command);
        let stdin = process.child.stdin.take();
        Self { process, stdin, read: 0, asked: 0, next_id: 0 }
    }

    pub fn send(&mut self, message: &Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{message}").unwrap();
    }

    /// Sends a request without waiting for its response.
    pub fn post(&mut self, method: &str, params: Value) {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    /// The next line on stdout: the response to the oldest unanswered request.
    pub fn response(&mut self) -> Value {
        let response = self.line();
        if !response["id"].is_null() {
            let id = (self.read - self.asked) as u64;
            assert_eq!(response["id"], id, "responses come in order: {response}");
        }
        response
    }

    /// The next line on stdout, which must be a request from the adapter.
    pub fn asked(&mut self) -> Value {
        self.asked += 1;
        let request = self.line();
        assert!(request["method"].is_string(), "a request: {request}");
        request
    }

    fn line(&mut self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(line) = self.process.stdout().lines().nth(self.read) {
                self.read += 1;
                return serde_json::from_str(line).unwrap();
            }
            assert!(Instant::now() < deadline, "no response: {}", self.process.stderr());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.post(method, params);
        self.response()
    }

    pub fn handshake(&mut self) {
        let reply = self.request("initialize", init("2025-11-25"));
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
        let instructions = reply["result"]["instructions"].as_str().unwrap_or_default();
        assert!(instructions.contains("memory_search"), "server instructions: {reply}");
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    pub fn post_call(&mut self, tool: &str, arguments: Value) {
        self.post("tools/call", json!({"name": tool, "arguments": arguments}));
    }

    /// Calls a tool and returns its result.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.post_call(tool, arguments);
        self.response()["result"].clone()
    }

    /// Closes stdin and waits for the exit.
    pub fn close(&mut self) -> (ExitStatus, String) {
        self.stdin = None;
        self.process.wait()
    }
}

pub fn init(version: &str) -> Value {
    json!({"protocolVersion": version, "capabilities": {},
           "clientInfo": {"name": "test", "version": "1"}})
}

pub fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

/// Polls `done` every 10 ms for up to 30 s.
pub fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `atticdb init` in `dir`. Returns the token.
pub fn init_config(dir: &Dir) -> String {
    let (status, token) =
        Process::spawn(&dir.0, &["init", "--config", "c.json", "--tenant", "a"]).wait();
    assert!(status.success(), "{token}");
    token.trim().to_owned()
}

pub fn locked(data: &Path) -> bool {
    File::open(data.join("LOCK")).is_ok_and(|file| file.try_lock().is_err())
}

/// Sends SIGKILL to the pid in `data`'s `LOCK`. Returns the pid if the signal was sent.
pub fn kill_lock_pid(data: &Path) -> Option<i32> {
    let lock = std::fs::read_to_string(data.join("LOCK")).ok()?;
    let pid = lock.strip_prefix("pid=")?.split(' ').next()?.parse().ok()?;
    let target = rustix::process::Pid::from_raw(pid)?;
    rustix::process::kill_process(target, rustix::process::Signal::KILL).ok()?;
    Some(pid)
}

/// Kills the server holding `data`'s lock, found through `LOCK`, and waits until the
/// lock is free. Auto-started servers outlive their adapters, so tests stop them here.
pub fn kill_holder(data: &Path) -> bool {
    if locked(data) {
        kill_lock_pid(data);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while locked(data) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// The pid in `data`'s `LOCK`.
fn lock_pid(data: &Path) -> Option<i32> {
    let lock = std::fs::read_to_string(data.join("LOCK")).ok()?;
    lock.strip_prefix("pid=")?.split(' ').next()?.parse().ok()
}

/// Whether `pid`'s command line runs `atticdb serve` on the data directory `data`.
fn serves(pid: i32, data: &Path) -> bool {
    let out = Command::new("/bin/ps").args(["-o", "command=", "-p", &pid.to_string()]).output();
    out.is_ok_and(|out| {
        let line = String::from_utf8_lossy(&out.stdout);
        line.contains("atticdb serve ") && line.contains(&format!("--data-dir {} ", data.display()))
    })
}

/// How long no lock may be held before [`stop_servers`] counts the servers as stopped.
const SETTLE: Duration = Duration::from_millis(500);

/// Stops the home servers of the data directories `dirs`: each one named in a `LOCK`, and only
/// when its command line runs `atticdb serve` on that directory. A server that another
/// process started may take the lock just after, so it watches until no lock was held for
/// [`SETTLE`], for up to 15 s.
pub fn stop_servers(dirs: &[PathBuf]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut free_since = Instant::now();
    while Instant::now() < deadline {
        let mut held = false;
        for data in dirs.iter().filter(|data| locked(data)) {
            held = true;
            let pid = lock_pid(data).filter(|pid| serves(*pid, data));
            if let Some(target) = pid.and_then(rustix::process::Pid::from_raw) {
                let _ = rustix::process::kill_process(target, rustix::process::Signal::KILL);
            }
        }
        if held {
            free_since = Instant::now();
        } else if free_since.elapsed() >= SETTLE {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Stops the home servers of these data directories at the end of a test, also when it
/// fails, by [`stop_servers`].
pub struct Holders(pub Vec<PathBuf>);

impl Drop for Holders {
    fn drop(&mut self) {
        stop_servers(&self.0);
    }
}

/// A new mode 0600 file in `dir`. Returns its path.
pub fn private_file(dir: &Dir, name: &str, content: &str) -> String {
    let path = dir.0.join(name);
    let mut file = File::options().write(true).create_new(true).mode(0o600).open(&path).unwrap();
    file.write_all(content.as_bytes()).unwrap();
    path.to_str().unwrap().to_owned()
}

/// The exit code, stdout and stderr of a finished `atticdb`.
pub type Output = (Option<i32>, String, String);

/// Runs `atticdb` in `cwd` with `HOME` set to `home` and `env` added, and writes `stdin` after
/// `delay`, keeping it open until the exit. A stray `ATTICDB_TOKEN` is set, which the home's
/// users ignore.
pub fn atticdb(
    home: &Path,
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: &[u8],
    delay: u64,
) -> Output {
    let mut command = command(env!("CARGO_BIN_EXE_atticdb"));
    command.args(args).current_dir(cwd).env("HOME", home).stdin(Stdio::piped());
    command.env("ATTICDB_TOKEN", "x").envs(env.iter().copied());
    let mut process = Process::start(&mut command);
    let mut input = process.child.stdin.take().unwrap();
    std::thread::sleep(Duration::from_millis(delay));
    // The hook reads at most 1 MiB, so a longer write fails once it exits.
    let _ = input.write_all(stdin);
    let (status, _) = process.wait();
    (status.code(), process.stdout(), process.stderr())
}

/// The hook's stdout and stderr, with `env` added, after it exits 0 with 1 line.
pub fn hook(home: &Path, stdin: &str, delay: u64, env: &[(&str, &str)]) -> (String, String) {
    let args = ["hook", "session-start"];
    let (code, out, err) = atticdb(home, home, &args, env, stdin.as_bytes(), delay);
    assert_eq!(code, Some(0), "{stdin:.80}: {err}");
    assert_eq!(out.lines().count(), 1, "{stdin:.80}: 1 line: {out}");
    (out, err)
}

/// Runs a verb on the home, which must succeed.
pub fn seed(home: &Path, args: &[&str]) {
    let (code, _, err) = atticdb(home, home, args, &[], b"", 0);
    assert_eq!(code, Some(0), "{args:?}: {err}");
}

/// The hook's stdin for a chat in `path`.
pub fn cwd(path: &Path) -> String {
    json!({"cwd": path}).to_string()
}

/// The home's data directory when `HOME` is `home`.
pub fn data_dir(home: &Path) -> PathBuf {
    let data =
        if cfg!(target_os = "macos") { "Library/Application Support" } else { ".local/share" };
    home.join(data).join("atticdb/data")
}

/// A fake store on `socket`. `/healthz` answers at once with instance `X`. Any other request
/// gets an empty read page after `delay`. With no `delay` it is never answered, and its
/// connection stays open until the client closes it. Returns a count of those requests.
pub fn slow_store(socket: &Path, delay: Option<Duration>) -> Arc<AtomicUsize> {
    let listener = UnixListener::bind(socket).unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let counted = counted.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut head = String::new();
                    while !head.ends_with("\r\n\r\n") {
                        if reader.read_line(&mut head).unwrap_or(0) == 0 {
                            return;
                        }
                    }
                    let body = if head.starts_with("GET /healthz ") {
                        r#"{"instance": "X"}"#
                    } else {
                        counted.fetch_add(1, Ordering::SeqCst);
                        let Some(delay) = delay else {
                            let _ = std::io::copy(&mut reader, &mut std::io::sink());
                            return;
                        };
                        std::thread::sleep(delay);
                        r#"{"facts": [], "next": null, "turns": [], "truncated": false}"#
                    };
                    let length = body.len();
                    let reply =
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n{body}");
                    if stream.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    reads
}
