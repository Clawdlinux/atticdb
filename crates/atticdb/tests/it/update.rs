//! The daily update check. `doctor` and `setup` ask a fake releases API on 127.0.0.1 with the
//! real `curl`. They cache the answer in the home. The hook and `mcp` never ask. A source
//! build reports `+dev` and never prints the notice. So these assert the request and the
//! cache. A release build also gets the notice line checked.

use crate::support::{self, data_dir, Dir, Holders, Mcp, Process};
use serde_json::{json, Value};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const OFF: &str = "ATTICDB_NO_UPDATE_CHECK";
const API: &str = "ATTICDB_RELEASES_API";
const CACHE: &str = "update-check.json";
const TAG: &str = r#"{"tag_name": "v9.9.9", "name": "AtticDB 9.9.9"}"#;

/// The exit code, stdout and stderr of a finished `atticdb`.
type Output = (Option<i32>, String, String);

/// A fake releases API on 127.0.0.1 that answers each request with `status` and `body`.
/// Returns its URL and a count of connections, so it also records any connection at all.
fn fake_api(status: u16, body: &str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/repos/x/releases/latest", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let (counted, body) = (hits.clone(), body.to_owned());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            let length = body.len();
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (url, hits)
}

fn hits(count: &AtomicUsize) -> usize {
    count.load(Ordering::SeqCst)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// A test's dirs: `HOME` at `h`, the AtticDB home at `home`, and a `PATH` holding only a link
/// to the real `curl`, so no harness is found.
struct Setup {
    dir: Dir,
    path: OsString,
}

impl Setup {
    fn new() -> Self {
        let dir = Dir::new();
        std::fs::create_dir(dir.0.join("h")).unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let curl = std::env::split_paths(&path).map(|dir| dir.join("curl")).find(|p| p.is_file());
        let curl = curl.unwrap_or_else(|| panic!("these tests need curl on PATH"));
        let bin = dir.0.join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(curl, bin.join("curl")).unwrap();
        Self { dir, path: bin.into() }
    }

    /// The AtticDB home, which no test makes until [`Self::make_home`].
    fn home(&self) -> PathBuf {
        self.dir.0.join("home")
    }

    /// Makes the home's directory, mode 0700, with no config. Doctor says it isn't set up.
    fn make_home(&self) {
        std::fs::DirBuilder::new().mode(0o700).create(self.home()).unwrap();
    }

    fn cache(&self) -> PathBuf {
        self.home().join(CACHE)
    }

    /// Runs `atticdb args` with the check on and the API at `url`, `env` added, and `stdin`
    /// written then closed. Stdout is [`steady`].
    fn run(&self, url: &str, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        self.run_on(&self.path, url, args, env, stdin)
    }

    /// [`Self::run`] with `PATH` set to `path`.
    fn run_on(
        &self,
        path: &OsStr,
        url: &str,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> Output {
        let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
        command.args(args).current_dir(&self.dir.0).env("HOME", self.dir.0.join("h"));
        command.env("ATTICDB_HOME", self.home()).env("PATH", path).env(API, url);
        command.env_remove(OFF).env_remove("CI");
        command.env_remove("ATTICDB_TOKEN").env_remove("ATTICDB_SETUP_INTERACTIVE");
        command.envs(env.iter().copied()).stdin(Stdio::piped());
        let mut process = Process::start(&mut command);
        let mut input = process.child.stdin.take().unwrap();
        let _ = input.write_all(stdin.as_bytes());
        drop(input);
        let (status, _) = process.wait();
        (status.code(), steady(&process.stdout()), process.stderr())
    }

    /// `atticdb doctor` with the check off.
    fn doctor_off(&self, url: &str) -> Output {
        self.run(url, &["doctor"], &[(OFF, "1")], "")
    }

    fn read_cache(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.cache()).unwrap()).unwrap()
    }
}

/// `out` with doctor's free space masked, as it changes between runs.
fn steady(out: &str) -> String {
    let free = |line: &str| line.starts_with("  free space: ");
    out.lines()
        .map(
            |line| if free(line) { "  free space: <masked>\n".into() } else { format!("{line}\n") },
        )
        .collect()
}

/// The notice for `latest`, which this binary prints only when it isn't a source build.
fn notice(latest: &str) -> Option<String> {
    let current = atticdb::VERSION;
    let shown = format!("AtticDB {latest} is out (you have {current}). Run: atticdb upgrade");
    (!current.ends_with("+dev")).then_some(shown)
}

/// With the check off, `out` plus the notice for `latest`, when this build prints one.
fn with_notice(out: &str, latest: &str) -> String {
    notice(latest).map_or_else(|| out.to_owned(), |line| format!("{out}{line}\n"))
}

/// Doctor asks the API once and writes the cache, mode 0600, with no temp file left. It
/// prints the notice as its last line, unless this is a source build. Its exit code and
/// stderr are the same as with the check off.
#[test]
fn doctor_shows_notice() {
    let setup = Setup::new();
    setup.make_home();
    let (url, count) = fake_api(200, TAG);
    let (code_off, out_off, err_off) = setup.doctor_off(&url);
    assert_eq!(hits(&count), 0, "{out_off}{err_off}");
    assert!(!setup.cache().exists());

    let before = now();
    let (code, out, err) = setup.run(&url, &["doctor"], &[], "");
    let after = now();
    assert_eq!(hits(&count), 1, "{out}{err}");
    assert_eq!((code, err.as_str()), (code_off, err_off.as_str()), "{out}");
    assert_eq!(out, with_notice(&out_off, "9.9.9"));
    let cache = setup.read_cache();
    assert_eq!(cache["latest"], "9.9.9", "{cache}");
    let checked = cache["checked"].as_u64().unwrap();
    assert!((before..=after).contains(&checked), "{cache}");
    assert_eq!(cache.as_object().unwrap().len(), 2, "{cache}");
    assert_eq!(std::fs::metadata(setup.cache()).unwrap().mode() & 0o777, 0o600);
    let names: Vec<_> = std::fs::read_dir(setup.home()).unwrap().map(|e| e.unwrap()).collect();
    assert_eq!(names.len(), 1, "only the cache: {names:?}");
}

/// Doctor on a home that doesn't exist still checks, but never makes the home.
#[test]
fn doctor_never_makes_the_home() {
    let setup = Setup::new();
    let (url, count) = fake_api(200, TAG);
    let (code_off, out_off, _) = setup.doctor_off(&url);
    let (code, out, err) = setup.run(&url, &["doctor"], &[], "");
    assert_eq!(hits(&count), 1, "{out}{err}");
    assert_eq!(code, code_off);
    assert_eq!(out, with_notice(&out_off, "9.9.9"));
    assert!(!setup.home().exists(), "{out}");
}

/// A second doctor within 24 h reads the cache and makes no request. A day-old cache is
/// checked again and rewritten.
#[test]
fn doctor_uses_cache() {
    let setup = Setup::new();
    setup.make_home();
    let (url, count) = fake_api(200, TAG);
    let (code_off, out_off, _) = setup.doctor_off(&url);
    setup.run(&url, &["doctor"], &[], "");
    assert_eq!(hits(&count), 1);
    let (code, out, err) = setup.run(&url, &["doctor"], &[], "");
    assert_eq!(hits(&count), 1, "the cache answers: {out}{err}");
    assert_eq!(code, code_off);
    assert_eq!(out, with_notice(&out_off, "9.9.9"), "the notice comes from the cache");

    let old = now() - 25 * 60 * 60;
    std::fs::write(setup.cache(), json!({"checked": old, "latest": "9.9.9"}).to_string()).unwrap();
    let (url, count) = fake_api(200, r#"{"tag_name": "v10.0.0"}"#);
    let (_, out, err) = setup.run(&url, &["doctor"], &[], "");
    assert_eq!(hits(&count), 1, "a day-old cache is checked again: {out}{err}");
    assert_eq!(out, with_notice(&out_off, "10.0.0"));
    assert_eq!(setup.read_cache()["latest"], "10.0.0");
    assert!(setup.read_cache()["checked"].as_u64().unwrap() > old);
}

/// A failed check prints nothing more, not even on stderr. It caches only the time it ran.
/// The failures are an HTTP error, bad JSON and no `tag_name`. So are a bad tag, no server
/// and no `curl`.
#[test]
fn doctor_quiet_when_check_fails() {
    let setup = Setup::new();
    setup.make_home();
    let off = setup.doctor_off("http://127.0.0.1:9/");
    let failed = |why: &str| {
        let cache = setup.read_cache();
        assert_eq!(cache.as_object().unwrap().len(), 1, "{why}: {cache}");
        assert!(now() - cache["checked"].as_u64().unwrap() < 60, "{why}: {cache}");
        std::fs::remove_file(setup.cache()).unwrap();
    };
    let rows = [
        (500, TAG),
        (200, "{"),
        (200, r#"{"name": "v9.9.9"}"#),
        (200, r#"{"tag_name": "nightly"}"#),
        (200, r#"{"tag_name": "v1.2.3333333333"}"#),
    ];
    for (status, body) in rows {
        let (url, count) = fake_api(status, body);
        let found = setup.run(&url, &["doctor"], &[], "");
        assert_eq!(hits(&count), 1, "{status} {body}");
        assert_eq!(found, off, "{status} {body}");
        failed(&format!("{status} {body}"));
    }
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let refused = format!("http://127.0.0.1:{port}/repos/x/releases/latest");
    assert_eq!(setup.run(&refused, &["doctor"], &[], ""), off, "no server");
    failed("no server");

    let (url, count) = fake_api(200, TAG);
    let empty = setup.dir.0.join("empty");
    std::fs::create_dir(&empty).unwrap();
    assert_eq!(setup.run_on(empty.as_os_str(), &url, &["doctor"], &[], ""), off, "no curl");
    assert_eq!(hits(&count), 0);
    failed("no curl");
}

/// After a failed check, a second doctor within the hour makes no request. An entry with no
/// `latest`, checked 2 h ago, is checked again.
#[test]
fn doctor_backs_off_after_a_failure() {
    let setup = Setup::new();
    setup.make_home();
    let (url, count) = fake_api(500, TAG);
    let off = setup.doctor_off(&url);
    assert_eq!(setup.run(&url, &["doctor"], &[], ""), off);
    assert_eq!(hits(&count), 1);
    assert_eq!(setup.read_cache().as_object().unwrap().len(), 1);

    let (url, count) = fake_api(200, TAG);
    assert_eq!(setup.run(&url, &["doctor"], &[], ""), off, "no notice while backing off");
    assert_eq!(hits(&count), 0, "no request within the hour");

    let old = now() - 2 * 60 * 60;
    std::fs::write(setup.cache(), json!({"checked": old}).to_string()).unwrap();
    let (code, out, err) = setup.run(&url, &["doctor"], &[], "");
    assert_eq!(hits(&count), 1, "checked again after 2 h: {out}{err}");
    assert_eq!((code, err), (off.0, off.2));
    assert_eq!(out, with_notice(&off.1, "9.9.9"));
    assert_eq!(setup.read_cache()["latest"], "9.9.9");
}

/// No notice when the latest release is this version or older. The check is still cached.
#[test]
fn no_notice_when_current() {
    let current = atticdb::VERSION.strip_suffix("+dev").unwrap_or(atticdb::VERSION);
    for latest in [current, "0.0.0"] {
        let setup = Setup::new();
        setup.make_home();
        let (url, count) = fake_api(200, &json!({"tag_name": format!("v{latest}")}).to_string());
        let off = setup.doctor_off(&url);
        assert_eq!(setup.run(&url, &["doctor"], &[], ""), off, "{latest}");
        assert_eq!(hits(&count), 1, "{latest}");
        assert_eq!(setup.read_cache()["latest"], latest);
    }
}

/// `ATTICDB_NO_UPDATE_CHECK=1`, or `CI` set to any value, turns the check off: no request,
/// no cache.
#[test]
fn no_check_when_disabled() {
    let setup = Setup::new();
    setup.make_home();
    let (url, count) = fake_api(200, TAG);
    for env in [(OFF, "1"), ("CI", "true"), ("CI", "1"), ("CI", "")] {
        let (_, out, err) = setup.run(&url, &["doctor"], &[env], "");
        assert_eq!(hits(&count), 0, "{env:?}: {out}{err}");
        assert!(!setup.cache().exists(), "{env:?}");
        assert!(!out.contains(" is out "), "{env:?}: {out}");
    }
}

/// Setup checks once it has applied, and prints the notice last. A dry run never checks.
#[test]
fn setup_checks_after_apply() {
    let setup = Setup::new();
    let home = setup.dir.0.join("h");
    std::fs::create_dir_all(home.join(".gemini/antigravity")).unwrap();
    std::fs::create_dir_all(home.join(".gemini/config")).unwrap();
    let _holders = Holders(vec![setup.home().join("data")]);
    let (url, count) = fake_api(200, TAG);
    let args = ["setup", "--yes", "--harness", "antigravity"];
    let (code, out, err) = setup.run(&url, &["setup", "--dry-run", "--yes"], &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(hits(&count), 0, "a dry run doesn't check: {out}");

    let (code, out, err) = setup.run(&url, &args, &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(hits(&count), 1, "{out}{err}");
    assert_eq!(setup.read_cache()["latest"], "9.9.9");
    let last = out.lines().last().unwrap();
    match notice("9.9.9") {
        Some(line) => assert_eq!(last, line, "{out}"),
        None => assert!(last.starts_with("See what your agent gets at chat start: "), "{out}"),
    }
    assert!(!err.contains("curl") && !err.contains(" is out "), "{err}");
}

/// The hook and `mcp` never check, even with the check on and the API set: the listener
/// sees no connection.
#[test]
fn hook_never_checks() {
    let setup = Setup::new();
    let _holders = Holders(vec![setup.home().join("data"), data_dir(&setup.dir.0.join("h"))]);
    let (url, count) = fake_api(200, TAG);
    let stdin = json!({"cwd": setup.dir.0}).to_string();
    for hook in ["session-start", "post-compact"] {
        let (code, out, err) = setup.run(&url, &["hook", hook], &[], &stdin);
        assert_eq!(code, Some(0), "{hook}: {out}{err}");
        assert_eq!(hits(&count), 0, "{hook}: {out}{err}");
    }
    assert!(!setup.cache().exists());

    let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
    command.arg("mcp").current_dir(&setup.dir.0).env("HOME", setup.dir.0.join("h"));
    command.env("ATTICDB_HOME", setup.home()).env("PATH", &setup.path).env(API, &url);
    command.env_remove(OFF).env_remove("CI").env_remove("ATTICDB_TOKEN");
    let mut mcp = Mcp::spawn(command.stdin(Stdio::piped()));
    mcp.handshake();
    let tools = mcp.request("tools/list", json!({}))["result"]["tools"].clone();
    assert!(!tools.as_array().unwrap().is_empty(), "{tools}");
    assert_eq!(mcp.close().0.code(), Some(0));
    assert_eq!(hits(&count), 0, "mcp never checks");
    assert!(!setup.cache().exists());
}
