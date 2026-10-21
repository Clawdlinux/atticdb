use crate::support::{Dir, Process, Via};
use serde_json::json;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

struct Ctx {
    dir: Dir,
    config: PathBuf,
    token: String,
    data: PathBuf,
    socket: PathBuf,
    /// Another server that must keep working after the row.
    other: Option<(Process, String)>,
}

impl Ctx {
    fn path(&self, name: &str) -> String {
        self.dir.0.join(name).to_str().unwrap().to_owned()
    }

    fn serve(&self, data: &Path, extra: &[&str]) -> Process {
        let (config, data) = (self.config.to_str().unwrap(), data.to_str().unwrap());
        let args = ["serve", "--config", config, "--data-dir", data];
        let listen =
            if extra.contains(&"--listen") { &[][..] } else { &["--listen", "127.0.0.1:0"] };
        Process::spawn(&[&args[..], listen, extra].concat())
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
        let mut first = self.serve(&self.data.clone(), &[]);
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
    Serves,
    /// `--exit-on-poison` with the fault trigger: the write gets unknown and the server exits 70.
    #[cfg(feature = "fault-inject")]
    ExitsOnPoison,
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
        ("socket mode 0600", |_| vec![], Expect::Serves),
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
            "group-writable parent",
            |c| {
                c.private_data_dir(0o770);
                vec![]
            },
            Expect::Fails(1, "not writable by group or others"),
        ),
        (
            "path past sun_path",
            |c| vec!["--unix-socket".into(), c.path(&"x".repeat(zerodb::listen::SUN_PATH_BYTES))],
            Expect::Fails(1, "set --unix-socket to a shorter path"),
        ),
        (
            "leftover staging directory",
            |c| {
                c.private_data_dir(0o700);
                let leftover = c.data.join(".zerodb-sock.0123456789abcdef0123456789abcdef");
                std::fs::create_dir(&leftover).unwrap();
                std::fs::write(leftover.join("s"), "").unwrap();
                vec![]
            },
            Expect::Serves,
        ),
        (
            "second serve on a locked data dir",
            |c| {
                c.other(&c.data.clone());
                vec![]
            },
            Expect::Fails(75, "is locked by pid="),
        ),
        #[cfg(feature = "fault-inject")]
        ("exit on poison", |_| vec!["--exit-on-poison".into()], Expect::ExitsOnPoison),
    ];

    let init = |config: &Path| {
        Process::spawn(&["init", "--config", config.to_str().unwrap(), "--tenant", "a"]).wait()
    };
    for (case, prepare, expect) in rows {
        let dir = Dir::new();
        let config = dir.0.join("c.json");
        let (status, token) = init(&config);
        assert!(status.success(), "{case}: init {token}");
        if case.starts_with("config mode") {
            let (status, again) = init(&config);
            assert!(!status.success() && again.contains("cannot create"), "init never overwrites");
        }
        let data = dir.0.join("d");
        let socket = data.join(zerodb::listen::SOCKET_NAME);
        let token = token.trim().to_owned();
        let mut ctx = Ctx { dir, config, token, data, socket, other: None };
        let extra = prepare(&mut ctx);
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        let mut server = ctx.serve(&ctx.data, &extra);
        match *expect {
            Expect::Fails(code, fragment) => {
                let (status, stderr) = server.wait();
                assert_eq!(status.code(), Some(code), "{case}: {stderr}");
                assert!(stderr.contains(fragment), "{case}: {stderr}");
                for secret in [ctx.token.as_str(), "secret-token"] {
                    assert!(!stderr.contains(secret), "{case}: stderr leaks a token");
                }
            }
            Expect::Serves => {
                let (instance, tcp) = server.ready();
                let meta = std::fs::symlink_metadata(&ctx.socket).unwrap();
                let group = extra.iter().position(|arg| *arg == "--unix-socket-group");
                let mode = if group.is_some() { 0o660 } else { 0o600 };
                assert_eq!(meta.mode() & 0o777, mode, "{case}: socket mode");
                if let Some(at) = group {
                    assert_eq!(meta.gid().to_string(), extra[at + 1], "{case}: socket group");
                }
                let lock = std::fs::read_to_string(ctx.data.join("LOCK")).unwrap();
                let address =
                    format!("instance={instance} unix={} tcp={tcp}", ctx.socket.display());
                assert!(lock.contains(&address), "{case}: LOCK {lock:?}");
                let unix = Via::Unix(ctx.socket.clone());
                let health = json!({"status": "ok", "instance": instance});
                assert_eq!(unix.call(None, None, "GET", "/healthz", None), (200, health), "{case}");
                let body = crate::support::append_body("s", "k", "v");
                let tcp = Via::Tcp(tcp);
                let (status, _) = tcp.call(
                    Some("127.0.0.1"),
                    Some(&ctx.token),
                    "POST",
                    "/v1/append",
                    Some(&body),
                );
                assert_eq!(status, 200, "{case}: the init token works");
                let staging = std::fs::read_dir(&ctx.data).unwrap().flatten().filter(|entry| {
                    entry.file_name().to_string_lossy().starts_with(".zerodb-sock.")
                });
                assert_eq!(staging.count(), 0, "{case}: no staging directory left");
            }
            #[cfg(feature = "fault-inject")]
            Expect::ExitsOnPoison => {
                let (_, tcp) = server.ready();
                std::fs::write(ctx.data.join("FAULT-SYNC-ONCE"), "").unwrap();
                let body = crate::support::append_body("s", "poison", "v");
                let tcp = Via::Tcp(tcp);
                let call = || {
                    tcp.call(Some("127.0.0.1"), Some(&ctx.token), "POST", "/v1/append", Some(&body))
                };
                assert_eq!(
                    call(),
                    (500, json!({"outcome": "unknown", "error": "storage"})),
                    "{case}"
                );
                let (status, stderr) = server.wait();
                assert_eq!(status.code(), Some(70), "{case}: {stderr}");
                assert!(!ctx.socket.exists(), "{case}: socket path removed");
                // The written bytes survived the exit, so a fresh start finds the key.
                let mut restarted = ctx.serve(&ctx.data, &[]);
                let (_, addr) = restarted.ready();
                let tcp = Via::Tcp(addr);
                let call = || {
                    tcp.call(Some("127.0.0.1"), Some(&ctx.token), "POST", "/v1/append", Some(&body))
                };
                let (status, retry) = call();
                assert_eq!(
                    (status, &retry["deduplicated"]),
                    (200, &json!(true)),
                    "{case}: {retry}"
                );
            }
        }
        if let Some((_, instance)) = &ctx.other {
            let health = Via::Unix(ctx.socket.clone()).call(None, None, "GET", "/healthz", None);
            assert_eq!(
                health,
                (200, json!({"status": "ok", "instance": instance})),
                "{case}: first server"
            );
        }
    }
}
