use crate::support::{append_body, Dir, Process, Via};
use serde_json::{json, Value};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

/// Spawned servers run in `dir` and get relative paths, so the socket's staging path
/// stays under `sun_path` however deep the checkout is.
struct Ctx {
    dir: Dir,
    config: PathBuf,
    token: String,
    /// `dir/d`, passed to the server as `d`.
    data: PathBuf,
    /// `dir/d/zerodb.sock`.
    socket: PathBuf,
    /// Another server that must keep working after the row.
    other: Option<(Process, String)>,
}

impl Ctx {
    fn run(&self, args: &[&str]) -> Process {
        Process::spawn(&self.dir.0, args)
    }

    fn serve(&self, data: &str, extra: &[&str]) -> Process {
        let args = ["serve", "--config", self.config.to_str().unwrap(), "--data-dir", data];
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
        let mut first = self.serve("d", &[]);
        first.ready();
        first.child.kill().unwrap();
        first.wait();
        assert!(self.socket.exists(), "a killed server leaves its socket");
    }

    /// Keeps a server on `data` with this row's socket path running.
    fn other(&mut self, data: &str) {
        let mut other = self.serve(data, &["--unix-socket", "d/zerodb.sock"]);
        let (instance, _) = other.ready();
        self.other = Some((other, instance));
    }
}

enum Expect {
    /// Exit code and a fragment of stderr.
    Fails(i32, &'static str),
    /// Serves, and these entries of the data directory still exist.
    Serves(&'static [&'static str]),
    /// `zerodb recover` refuses while the server runs, then cuts after the first write.
    Recovers,
    /// `--exit-on-poison` with the fault trigger: the write gets unknown and the server exits 70.
    #[cfg(feature = "fault-inject")]
    ExitsOnPoison,
}

const TENANT: &str = r#"{"name": "a", "token": "secret-token-0123456789abcdefghijkl"}"#;
const TENANT_B: &str = r#"{"name": "b", "token": "secret-token-0123456789abcdefghijkX"}"#;
const FOREIGN: &str = ".zerodb-sock.other.sock.0123456789abcdef0123456789abcdef";

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
                let status = std::process::Command::new("mkfifo")
                    .args(["-m", "600"])
                    .arg(&c.config)
                    .status();
                assert!(status.unwrap().success());
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
            Expect::Fails(1, "at least 32 bytes"),
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
            "non-loopback tcp",
            |_| vec!["--listen".into(), "0.0.0.0:0".into()],
            Expect::Fails(1, zerodb::listen::NEEDS_TLS),
        ),
        ("socket mode 0600", |_| vec![], Expect::Serves(&[])),
        ("no tcp", |_| vec!["--listen".into(), "none".into()], Expect::Serves(&[])),
        (
            "two tenants",
            |c| {
                let tenant = format!(r#"{{"name": "a", "token": "{}"}}"#, c.token);
                c.write_config(&format!(r#"{{"tenants": [{tenant}, {TENANT_B}]}}"#), 0o600);
                vec![]
            },
            Expect::Serves(&[]),
        ),
        (
            "socket group 0660",
            |c| {
                let gid = std::fs::metadata(&c.dir.0).unwrap().gid();
                vec!["--unix-socket-group".into(), gid.to_string()]
            },
            Expect::Serves(&[]),
        ),
        (
            "stale socket from a killed server",
            |c| {
                c.stale_socket();
                vec![]
            },
            Expect::Serves(&[]),
        ),
        (
            "live socket of another server",
            |c| {
                c.private_data_dir(0o700);
                c.other("other");
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
            "group-writable parent",
            |c| {
                c.private_data_dir(0o770);
                vec![]
            },
            Expect::Fails(1, "not writable by group or others"),
        ),
        (
            "path past sun_path",
            |_| vec!["--unix-socket".into(), "x".repeat(zerodb::listen::SUN_PATH_BYTES)],
            Expect::Fails(1, "set --unix-socket to a shorter path"),
        ),
        (
            "leftover staging directory",
            |c| {
                c.private_data_dir(0o700);
                // A killed server's staging directory, and one of a server on another socket.
                for name in [".zerodb-sock.zerodb.sock.0123456789abcdef0123456789abcdef", FOREIGN] {
                    std::fs::create_dir(c.data.join(name)).unwrap();
                    std::fs::write(c.data.join(name).join("s"), "").unwrap();
                }
                vec![]
            },
            Expect::Serves(&[FOREIGN]),
        ),
        (
            "second serve on a locked data dir",
            |c| {
                c.other("d");
                vec![]
            },
            Expect::Fails(75, "is locked by pid="),
        ),
        #[cfg(feature = "fault-inject")]
        ("exit on poison", |_| vec!["--exit-on-poison".into()], Expect::ExitsOnPoison),
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
        let mut ctx = Ctx { dir, config, token, data, socket, other: None };
        let extra = prepare(&mut ctx);
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        let mut server = ctx.serve("d", &extra);
        let unix = Via::Unix(ctx.socket.clone());
        match *expect {
            Expect::Fails(code, fragment) => {
                let (status, stderr) = server.wait();
                assert_eq!(status.code(), Some(code), "{case}: {stderr}");
                assert!(stderr.contains(fragment), "{case}: {stderr}");
                for secret in [ctx.token.as_str(), "secret-token"] {
                    assert!(!stderr.contains(secret), "{case}: stderr leaks a token");
                }
            }
            Expect::Serves(kept) => {
                let (instance, tcp) = server.ready();
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
                let mut staging: Vec<String> = std::fs::read_dir(&ctx.data)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(".zerodb-sock."))
                    .collect();
                staging.sort();
                assert_eq!(staging, kept, "{case}: staging directories");
            }
            Expect::Recovers => {
                server.ready();
                let (status, first) = ctx.append(&unix, "one");
                assert_eq!(status, 200, "{case}: {first}");
                assert_eq!(ctx.append(&unix, "two").0, 200, "{case}");
                let cut = first["last_lsn"].to_string();
                let recover = ["recover", "--truncate-at", &cut, "--data-dir", "d"];
                let (status, stderr) = ctx.run(&recover).wait();
                assert_eq!(status.code(), Some(75), "{case}: refused under the lock: {stderr}");
                server.child.kill().unwrap();
                server.wait();
                let (status, report) = ctx.run(&recover).wait();
                assert!(status.success(), "{case}: {report}");
                let mut restarted = ctx.serve("d", &[]);
                restarted.ready();
                let target = "/v1/window?agent=a&session=s&last=10";
                let (_, window) = unix.call(None, Some(&ctx.token), "GET", target, None);
                let contents: Vec<&Value> =
                    window["turns"].as_array().unwrap().iter().map(|t| &t["content"]).collect();
                assert_eq!(contents, [&json!("one")], "{case}: only turns at or below the cut");
            }
            #[cfg(feature = "fault-inject")]
            Expect::ExitsOnPoison => {
                server.ready();
                std::fs::write(ctx.data.join("FAULT-SYNC-ONCE"), "").unwrap();
                let unknown = json!({"outcome": "unknown", "error": "storage"});
                assert_eq!(ctx.append(&unix, "poison"), (500, unknown), "{case}");
                let (status, stderr) = server.wait();
                assert_eq!(status.code(), Some(70), "{case}: {stderr}");
                assert!(!ctx.socket.exists(), "{case}: socket path removed");
                // The written bytes survived the exit, so a fresh start finds the key.
                let mut restarted = ctx.serve("d", &[]);
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
