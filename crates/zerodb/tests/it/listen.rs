use crate::support::{self, append_body, Dir, Process, Via};
use serde_json::{json, Value};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

struct Ctx {
    dir: Dir,
    config: PathBuf,
    token: String,
    data: PathBuf,
    /// `dir/d/zerodb.sock`.
    socket: PathBuf,
    /// Another server that must keep working after the row.
    other: Option<(Process, String)>,
    /// Sockets the row keeps open, such as a saturated listener.
    held: Vec<OwnedFd>,
}

impl Ctx {
    fn path(&self, name: &str) -> String {
        self.dir.0.join(name).to_str().unwrap().to_owned()
    }

    fn run(&self, args: &[&str]) -> Process {
        Process::spawn(&self.dir.0, args)
    }

    fn serve(&self, data: &Path, extra: &[&str]) -> Process {
        let (config, data) = (self.config.to_str().unwrap(), data.to_str().unwrap());
        let args = ["serve", "--config", config, "--data-dir", data];
        let listen =
            if extra.contains(&"--listen") { &[][..] } else { &["--listen", "127.0.0.1:0"] };
        self.run(&[&args[..], listen, extra].concat())
    }

    fn append(&self, via: &Via, key: &str) -> (u16, Value) {
        let body = append_body("s", key, key);
        via.call(Some("127.0.0.1"), Some(&self.token), "POST", "/v1/append", Some(&body))
    }

    fn write_config(&self, body: &str, mode: u32) {
        std::fs::remove_file(&self.config).unwrap();
        std::fs::write(&self.config, body).unwrap();
        std::fs::set_permissions(&self.config, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn private_data_dir(&self, mode: u32) {
        std::fs::create_dir(&self.data).unwrap();
        std::fs::set_permissions(&self.data, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Starts a server on this data directory, kills it, and leaves its socket behind.
    fn stale_socket(&mut self) {
        let mut first = self.serve(&self.data, &[]);
        first.ready();
        first.child.kill().unwrap();
        first.wait();
        assert!(self.socket.exists(), "a killed server leaves its socket");
    }

    /// Keeps a server on `data` with this row's socket path running.
    fn other(&mut self, data: &Path) {
        let mut other = self.serve(data, &["--unix-socket", self.socket.to_str().unwrap()]);
        let (instance, _) = other.ready();
        self.other = Some((other, instance));
    }
}

enum Expect {
    /// Exit code and a fragment of stderr.
    Fails(i32, &'static str),
    /// Serves, and leaves no staging directory behind.
    Serves,
    /// `zerodb recover` refuses while the server runs, then cuts after the first write.
    Recovers,
    /// Two servers start at once with one socket path: one serves, the other fails.
    OneWins,
    /// `--exit-on-poison` with the fault trigger: the write gets unknown and the server exits 70.
    /// With `true`, another server installed a socket at the path first, and it stays.
    #[cfg(feature = "fault-inject")]
    ExitsOnPoison(bool),
}

const TENANT: &str = r#"{"name": "a", "token": "secret-token-0123456789abcdefghijkl"}"#;
const TENANT_B: &str = r#"{"name": "b", "token": "secret-token-0123456789abcdefghijkX"}"#;

/// Drives the real binary through startup. Every row starts from a fresh directory with
/// a config made by `zerodb init`, and a data directory the server creates.
#[test]
fn startup_checks() {
    type Prepare = fn(&mut Ctx) -> Vec<String>;
    let rows: &[(&str, Prepare, Expect)] = &[
        (
            "config mode 0644",
            |c| {
                std::fs::set_permissions(&c.config, std::fs::Permissions::from_mode(0o644))
                    .unwrap();
                vec![]
            },
            Expect::Fails(1, "found mode 0644"),
        ),
        (
            "config is a FIFO",
            |c| {
                std::fs::remove_file(&c.config).unwrap();
                support::mkfifo(&c.config);
                vec![]
            },
            Expect::Fails(1, "must be a regular file"),
        ),
        (
            "duplicate tenant name",
            |c| {
                c.write_config(
                    &format!(
                        r#"{{"tenants": [{TENANT}, {}]}}"#,
                        TENANT_B.replace("\"b\"", "\"a\"")
                    ),
                    0o600,
                );
                vec![]
            },
            Expect::Fails(1, "tenants[1]: two tenants share a name"),
        ),
        (
            "duplicate token",
            |c| {
                c.write_config(
                    &format!(r#"{{"tenants": [{TENANT}, {}]}}"#, TENANT.replace("\"a\"", "\"b\"")),
                    0o600,
                );
                vec![]
            },
            Expect::Fails(1, "tenants[1]: two tenants share a token"),
        ),
        (
            "short token",
            |c| {
                c.write_config(r#"{"tenants": [{"name": "a", "token": "secret-token"}]}"#, 0o600);
                vec![]
            },
            Expect::Fails(1, "32 to 1024 bytes"),
        ),
        (
            "tenants as a map",
            |c| {
                c.write_config(
                    r#"{"tenants": {"a": "secret-token-0123456789abcdefghijkl"}}"#,
                    0o600,
                );
                vec![]
            },
            Expect::Fails(1, "must be an array"),
        ),
        (
            "extra tenant field",
            |c| {
                c.write_config(
                    &format!(r#"{{"tenants": [{}]}}"#, TENANT.replace('}', r#", "x": 1}"#)),
                    0o600,
                );
                vec![]
            },
            Expect::Fails(1, "exactly a string"),
        ),
        (
            "mcp token file mode 0644",
            |c| {
                let path = c.dir.0.join("t");
                std::fs::write(&path, &c.token).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
                let url = format!("unix://{}", c.socket.display());
                ["mcp", "--token-file", path.to_str().unwrap(), "--url", &url]
                    .map(Into::into)
                    .into()
            },
            Expect::Fails(1, "found mode 0644"),
        ),
        (
            "mcp url over non-loopback http",
            |_| ["mcp", "--url", "http://example.com:80"].map(Into::into).into(),
            Expect::Fails(1, "http sends the token in cleartext"),
        ),
        (
            "mcp url without a port",
            |_| ["mcp", "--url", "http://127.0.0.1"].map(Into::into).into(),
            Expect::Fails(1, "needs an explicit :<port>"),
        ),
        (
            "non-loopback tcp",
            |_| vec!["--listen".into(), "0.0.0.0:0".into()],
            Expect::Fails(1, zerodb::listen::NEEDS_TLS),
        ),
        ("socket mode 0600", |_| vec![], Expect::Serves),
        ("no tcp", |_| vec!["--listen".into(), "none".into()], Expect::Serves),
        (
            "two tenants",
            |c| {
                let tenant = format!(r#"{{"name": "a", "token": "{}"}}"#, c.token);
                c.write_config(&format!(r#"{{"tenants": [{tenant}, {TENANT_B}]}}"#), 0o600);
                vec![]
            },
            Expect::Serves,
        ),
        (
            "socket group 0660",
            |c| {
                let gid = std::fs::metadata(&c.dir.0).unwrap().gid();
                vec!["--unix-socket-group".into(), gid.to_string()]
            },
            Expect::Serves,
        ),
        (
            "stale socket from a killed server",
            |c| {
                c.stale_socket();
                vec![]
            },
            Expect::Serves,
        ),
        (
            "live socket of another server",
            |c| {
                c.private_data_dir(0o700);
                c.other(&c.dir.0.join("other"));
                vec![]
            },
            Expect::Fails(1, "another server is listening"),
        ),
        (
            "regular file at the socket path",
            |c| {
                c.private_data_dir(0o700);
                std::fs::write(&c.socket, "").unwrap();
                vec![]
            },
            Expect::Fails(1, "is not a socket"),
        ),
        (
            "group-writable socket directory",
            |c| {
                let parent = c.dir.0.join("g");
                std::fs::create_dir(&parent).unwrap();
                std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o770)).unwrap();
                c.socket = parent.join("s");
                vec!["--unix-socket".into(), c.socket.to_str().unwrap().into()]
            },
            Expect::Fails(1, "not writable by group or others"),
        ),
        (
            "existing data dir with mode 0755",
            |c| {
                c.private_data_dir(0o755);
                vec![]
            },
            Expect::Fails(1, "mode 0700 or stricter, found mode 0755"),
        ),
        (
            "two data dirs race for one socket path with a stale socket on it",
            |c| {
                c.socket = c.dir.0.join("shared.sock");
                drop(std::os::unix::net::UnixListener::bind(&c.socket).unwrap());
                let socket = c.socket.to_str().unwrap().to_owned();
                let other = c.serve(&c.dir.0.join("other"), &["--unix-socket", &socket]);
                c.other = Some((other, String::new()));
                vec!["--unix-socket".into(), socket]
            },
            Expect::OneWins,
        ),
        (
            "path past sun_path",
            |c| vec!["--unix-socket".into(), c.path(&"x".repeat(zerodb::listen::SUN_PATH_BYTES))],
            Expect::Fails(1, "sun_path limit; set --unix-socket to a shorter path"),
        ),
        (
            "staging path past sun_path",
            |c| {
                // The socket path fits, but `<parent>/.zs.<8 hex>/s` does not.
                let parent = c.dir.0.join("p".repeat(94 - c.dir.0.as_os_str().len()));
                std::fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
                c.socket = parent.join("x");
                assert!(c.socket.as_os_str().len() < 100 && parent.as_os_str().len() == 95);
                vec!["--unix-socket".into(), c.socket.to_str().unwrap().into()]
            },
            Expect::Fails(1, "sun_path limit; set --unix-socket to a shorter path"),
        ),
        (
            "second serve on a locked data dir",
            |c| {
                c.other(&c.data.clone());
                vec![]
            },
            Expect::Fails(75, "is locked by pid="),
        ),
        (
            "LOCK is a FIFO and the TCP port is taken",
            |c| {
                c.private_data_dir(0o700);
                support::mkfifo(&c.data.join("LOCK"));
                let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let listen = taken.local_addr().unwrap().to_string();
                c.held.push(taken.into());
                vec!["--listen".into(), listen]
            },
            Expect::Fails(1, "cannot listen on"),
        ),
        // macOS refuses a connect to a full backlog with ECONNREFUSED, as for a stale
        // socket, so only Linux can tell a saturated listener is live.
        #[cfg(target_os = "linux")]
        (
            "saturated listener at the socket path",
            |c| {
                c.private_data_dir(0o700);
                c.held = support::saturated(&c.socket);
                vec![]
            },
            Expect::Fails(1, "another server is listening"),
        ),
        (
            "second serve on the same fixed port and data dir",
            |c| {
                let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let listen = free.local_addr().unwrap().to_string();
                drop(free);
                let mut first = c.serve(&c.data, &["--listen", &listen]);
                let (instance, _) = first.ready();
                c.other = Some((first, instance));
                vec!["--listen".into(), listen]
            },
            Expect::Fails(75, "is locked by pid="),
        ),
        #[cfg(feature = "fault-inject")]
        ("exit on poison", |_| vec!["--exit-on-poison".into()], Expect::ExitsOnPoison(false)),
        #[cfg(feature = "fault-inject")]
        (
            "exit on poison keeps a socket another server installed later",
            |_| vec!["--exit-on-poison".into()],
            Expect::ExitsOnPoison(true),
        ),
        ("recover", |_| vec![], Expect::Recovers),
    ];

    for (case, prepare, expect) in rows {
        let dir = Dir::new();
        let config = dir.0.join("c.json");
        let init =
            || Process::spawn(&dir.0, &["init", "--config", "c.json", "--tenant", "a"]).wait();
        let (status, token) = init();
        assert!(status.success(), "{case}: init {token}");
        if case.starts_with("config mode") {
            let (status, again) = init();
            assert!(!status.success() && again.contains("cannot create"), "init never overwrites");
        }
        let data = dir.0.join("d");
        let socket = data.join(zerodb::listen::SOCKET_NAME);
        let token = token.trim().to_owned();
        let mut ctx = Ctx { dir, config, token, data, socket, other: None, held: vec![] };
        let extra = prepare(&mut ctx);
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        let mut server = match extra.first() {
            Some(&"mcp") => ctx.run(&extra),
            _ => ctx.serve(&ctx.data, &extra),
        };
        let unix = Via::Unix(ctx.socket.clone());
        match *expect {
            Expect::Fails(code, fragment) => {
                let (status, stderr) = server.wait();
                assert_eq!(status.code(), Some(code), "{case}: {stderr}");
                assert!(stderr.contains(fragment), "{case}: {stderr}");
                if case.starts_with("saturated") {
                    assert!(ctx.socket.exists(), "{case}: the live socket stays");
                }
                if case.starts_with("staging") {
                    let parent = std::fs::read_dir(ctx.socket.parent().unwrap()).unwrap();
                    assert_eq!(parent.count(), 0, "{case}: nothing is bound or staged");
                }
                for secret in [ctx.token.as_str(), "secret-token"] {
                    assert!(!stderr.contains(secret), "{case}: stderr leaks a token");
                }
            }
            Expect::OneWins => {
                let (other, _) = ctx.other.take().unwrap();
                let mut pair = [server, other];
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let lost = loop {
                    if let Some(lost) =
                        (0..2).find(|&i| pair[i].child.try_wait().unwrap().is_some())
                    {
                        break lost;
                    }
                    assert!(std::time::Instant::now() < deadline, "{case}: neither failed");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                };
                let (status, stderr) = pair[lost].wait();
                assert_eq!(status.code(), Some(1), "{case}: {stderr}");
                assert!(stderr.contains("another server is listening on"), "{case}: {stderr}");
                let (instance, _) = pair[1 - lost].ready();
                let health = json!({"status": "ok", "instance": instance});
                assert_eq!(unix.call(None, None, "GET", "/healthz", None), (200, health), "{case}");
            }
            Expect::Serves => {
                let (instance, tcp) = server.ready();
                let data = std::fs::metadata(&ctx.data).unwrap();
                assert_eq!(data.mode() & 0o777, 0o700, "{case}: data directory mode");
                let meta = std::fs::symlink_metadata(&ctx.socket).unwrap();
                let group = extra.iter().position(|arg| *arg == "--unix-socket-group");
                let mode = if group.is_some() { 0o660 } else { 0o600 };
                assert_eq!(meta.mode() & 0o777, mode, "{case}: socket mode");
                if let Some(at) = group {
                    assert_eq!(meta.gid().to_string(), extra[at + 1], "{case}: socket group");
                }
                let lock = std::fs::read_to_string(ctx.data.join("LOCK")).unwrap();
                let tcp_text = tcp.map_or("none".into(), |addr| addr.to_string());
                let address =
                    format!("instance={instance} unix={} tcp={tcp_text}", ctx.socket.display());
                assert!(lock.contains(&address), "{case}: LOCK {lock:?}");
                let health = json!({"status": "ok", "instance": instance});
                assert_eq!(unix.call(None, None, "GET", "/healthz", None), (200, health), "{case}");
                let via = tcp.map_or(unix.clone(), Via::Tcp);
                assert_eq!(ctx.append(&via, "k").0, 200, "{case}: the init token works");
                let left: Vec<String> = std::fs::read_dir(&ctx.data)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(".zs."))
                    .collect();
                assert!(left.is_empty(), "{case}: staging directories {left:?}");
            }
            Expect::Recovers => {
                server.ready();
                let (status, first) = ctx.append(&unix, "one");
                assert_eq!(status, 200, "{case}: {first}");
                let (status, second) = ctx.append(&unix, "two");
                assert_eq!(status, 200, "{case}: {second}");
                let cut = first["last_lsn"].to_string();
                let data = ctx.data.to_str().unwrap();
                let url = format!("unix://{}", ctx.socket.display());
                let token = support::private_file(&ctx.dir, "token", &ctx.token);
                let load = ["load", "--url", &url, "--token-file", &token];
                let good = ["--writers", "2", "--turns", "4", "--think", "1"];
                let (status, out) = ctx.run(&[&load[..], &good].concat()).wait();
                assert!(status.success(), "{case}: {out}");
                for line in ["turns=4", "think_ms=1"] {
                    assert!(out.lines().any(|got| got == line), "{case}: {line} in {out}");
                }
                assert!(out.lines().any(|line| line.starts_with("p99_us=")), "{case}: {out}");
                let huge = ["--writers", "9223372036854775808", "--batch", "2", "--turns", "4"];
                let slow = ["--turns", "4", "--think", "60001"];
                for bad in [&["--writers", "2", "--turns", "3"][..], &huge, &slow] {
                    let (status, out) = ctx.run(&[&load[..], bad].concat()).wait();
                    assert_eq!(status.code(), Some(2), "{case}: {bad:?} is a usage error: {out}");
                }
                let (status, stats) = ctx.run(&["stats", "--data-dir", data]).wait();
                assert!(status.success(), "{case}: {stats}");
                let last = second["last_lsn"].as_u64().unwrap() + 4;
                let durable = format!("durable_lsn={last}");
                for line in ["health=ok", &durable] {
                    assert!(stats.lines().any(|got| got == line), "{case}: {line} in {stats}");
                }
                let recover = ["recover", "--truncate-at", &cut, "--data-dir", data];
                let (status, stderr) = ctx.run(&recover).wait();
                assert_eq!(status.code(), Some(75), "{case}: refused under the lock: {stderr}");
                let verify = ["verify", "--data-dir", data];
                let (status, stderr) = ctx.run(&verify).wait();
                assert_eq!(status.code(), Some(75), "{case}: verify under the lock: {stderr}");
                server.child.kill().unwrap();
                server.wait();
                // A read-only backup must still verify.
                let modes = |mode| {
                    for name in support::files(&ctx.data).into_keys() {
                        let perms = std::fs::Permissions::from_mode(mode);
                        std::fs::set_permissions(ctx.data.join(name), perms).unwrap();
                    }
                };
                modes(0o400);
                let before = support::files(&ctx.data);
                let inside = ["--scratch", data];
                let (status, out) = ctx.run(&[&verify[..], &inside].concat()).wait();
                assert_eq!(status.code(), Some(2), "{case}: scratch inside the data dir: {out}");
                let (status, out) = ctx.run(&verify).wait();
                assert!(status.success(), "{case}: {out}");
                for line in ["health=ok", &durable] {
                    assert!(out.lines().any(|got| got == line), "{case}: {line} in {out}");
                }
                assert_eq!(support::files(&ctx.data), before, "{case}: verify wrote the data dir");
                let left =
                    support::files(&ctx.dir.0).into_keys().filter(|n| n.contains(".verify-"));
                assert_eq!(left.count(), 0, "{case}: verify left its copy");
                modes(0o600);
                let from = first["first_lsn"].as_u64().unwrap();
                let dump = ["dump", "--data-dir", data, "--from", &from.to_string()];
                let (status, out) = ctx.run(&dump).wait();
                assert!(status.success() && !out.contains("two"), "{case}: no content: {out}");
                let lsns: Vec<&str> = out.lines().map(|l| l.split(' ').next().unwrap()).collect();
                let acked: Vec<String> = (from..=last).map(|l| format!("lsn={l}")).collect();
                assert_eq!(lsns, acked, "{case}: acked records in LSN order: {out}");
                let (status, out) = ctx.run(&[&dump[..], &["--limit", "1"]].concat()).wait();
                assert!(status.success() && out.lines().count() == 1, "{case}: limit 1: {out}");
                let (status, stats) = ctx.run(&["stats", "--data-dir", data]).wait();
                assert_eq!(status.code(), Some(1), "{case}: no server: {stats}");
                for url in ["http://127.0.0.1:7425", "not-a-url"] {
                    let (status, stats) = ctx.run(&["stats", "--url", url]).wait();
                    assert_eq!(status.code(), Some(2), "{case}: {url} is a usage error: {stats}");
                }
                let (status, report) = ctx.run(&recover).wait();
                assert!(status.success(), "{case}: {report}");
                let mut restarted = ctx.serve(&ctx.data, &[]);
                restarted.ready();
                let target = "/v1/window?agent=a&session=s&last=10";
                let (_, window) = unix.call(None, Some(&ctx.token), "GET", target, None);
                let contents: Vec<&Value> =
                    window["turns"].as_array().unwrap().iter().map(|t| &t["content"]).collect();
                assert_eq!(contents, [&json!("one")], "{case}: only turns at or below the cut");
                restarted.child.kill().unwrap();
                restarted.wait();
                // Replay hides a bad checkpoint from the open, so verify reports it.
                let checkpoint = ctx.data.join("CHECKPOINT");
                std::fs::write(&checkpoint, "garbage").unwrap();
                let (status, out) = ctx.run(&verify).wait();
                assert_eq!(status.code(), Some(1), "{case}: rejected checkpoint: {out}");
                assert!(out.contains("checkpoint"), "{case}: {out}");
                std::fs::remove_file(&checkpoint).unwrap();
                // A symlink is refused, never skipped into an incomplete copy.
                std::os::unix::fs::symlink("elsewhere", &checkpoint).unwrap();
                let (status, out) = ctx.run(&verify).wait();
                assert_eq!(status.code(), Some(1), "{case}: symlinked checkpoint: {out}");
                assert!(out.contains("not a regular file"), "{case}: {out}");
                std::fs::remove_file(checkpoint).unwrap();
                // The restart put the superblock above LSN 1, so this is corruption, not a torn tail.
                let segment = ctx.data.join("seg-0000000000000001.zdb");
                let file = std::fs::OpenOptions::new().read(true).write(true).open(segment);
                let (file, mut byte) = (file.unwrap(), [0]);
                file.read_exact_at(&mut byte, 40).unwrap();
                file.write_all_at(&[!byte[0]], 40).unwrap();
                let (status, out) = ctx.run(&verify).wait();
                assert_eq!(status.code(), Some(1), "{case}: corrupt first frame: {out}");
                assert!(out.contains("at LSN 1,"), "{case}: names the LSN: {out}");
            }
            #[cfg(feature = "fault-inject")]
            Expect::ExitsOnPoison(replaced) => {
                let (_, tcp) = server.ready();
                let other = replaced.then(|| {
                    std::fs::remove_file(&ctx.socket).unwrap();
                    std::os::unix::net::UnixListener::bind(&ctx.socket).unwrap()
                });
                std::fs::write(ctx.data.join(zerodb::fault::TRIGGER), "").unwrap();
                let unknown = json!({"outcome": "unknown", "error": "storage"});
                assert_eq!(ctx.append(&Via::Tcp(tcp.unwrap()), "poison"), (500, unknown), "{case}");
                let (status, stderr) = server.wait();
                assert_eq!(status.code(), Some(70), "{case}: {stderr}");
                if other.is_some() {
                    let kept = std::os::unix::net::UnixStream::connect(&ctx.socket);
                    assert!(kept.is_ok(), "{case}: {kept:?}");
                    continue;
                }
                assert!(!ctx.socket.exists(), "{case}: socket path removed");
                // The written bytes survived the exit, so a fresh start finds the key.
                let mut restarted = ctx.serve(&ctx.data, &[]);
                restarted.ready();
                let (status, retry) = ctx.append(&unix, "poison");
                assert_eq!(
                    (status, &retry["deduplicated"]),
                    (200, &json!(true)),
                    "{case}: {retry}"
                );
            }
        }
        if let Some((_, instance)) = &ctx.other {
            let health = json!({"status": "ok", "instance": instance});
            assert_eq!(
                unix.call(None, None, "GET", "/healthz", None),
                (200, health),
                "{case}: first server"
            );
        }
    }
}
