//! The daily update check, and `atticdb upgrade`. `doctor` and `setup` ask a fake releases API
//! on 127.0.0.1 with the real `curl`. They cache the answer in the home. The hook and `mcp`
//! never ask. A source build reports `+dev` and never prints the notice. So these assert the
//! request and the cache. A release build also gets the notice line checked. `upgrade` runs on
//! a copy of the binary, with a stub `install.sh` from the fake release.

use crate::support::{self, data_dir, Dir, Holders, Mcp, Process};
use serde_json::{json, Value};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const OFF: &str = "ATTICDB_NO_UPDATE_CHECK";
const API: &str = "ATTICDB_RELEASES_API";
const CACHE: &str = "update-check.json";
const TAG: &str = r#"{"tag_name": "v9.9.9", "name": "AtticDB 9.9.9"}"#;

/// The exit code, stdout and stderr of a finished `atticdb`.
type Output = (Option<i32>, String, String);

/// A fake HTTP server on 127.0.0.1. Each request's path goes to `answer`, which gives the
/// status and body. It answers each connection, then closes it. Returns `http://<addr>`.
fn fake_http(answer: impl Fn(&str) -> (u16, String) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let mut path = None;
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                // The request line: `GET <path> HTTP/1.1`.
                path = path.or_else(|| line.split(' ').nth(1).map(str::to_owned));
                line.clear();
            }
            let (status, body) = answer(path.as_deref().unwrap_or_default());
            let length = body.len();
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    url
}

/// A fake releases API on 127.0.0.1 that answers each request with `status` and `body`.
/// Returns its URL and a count of requests.
fn fake_api(status: u16, body: &str) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let (counted, body) = (hits.clone(), body.to_owned());
    let url = fake_http(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        (status, body.clone())
    });
    (format!("{url}/repos/x/releases/latest"), hits)
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
        self.run_exe(env!("CARGO_BIN_EXE_atticdb").as_ref(), path, url, args, env, stdin)
    }

    /// [`Self::run_on`] with the binary at `exe`.
    fn run_exe(
        &self,
        exe: &Path,
        path: &OsStr,
        url: &str,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> Output {
        let mut process = self.start_exe(exe, path, url, args, env);
        let mut input = process.child.stdin.take().unwrap();
        let _ = input.write_all(stdin.as_bytes());
        drop(input);
        let (status, _) = process.wait();
        (status.code(), steady(&process.stdout()), process.stderr())
    }

    /// Starts [`Self::run_exe`]'s process, with stdin still open.
    fn start_exe(
        &self,
        exe: &Path,
        path: &OsStr,
        url: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Process {
        let mut command = support::command(exe);
        command.args(args).current_dir(&self.dir.0).env("HOME", self.dir.0.join("h"));
        command.env("ATTICDB_HOME", self.home()).env("PATH", path).env(API, url);
        // CURL_HOME and XDG_CONFIG_HOME would move curl's config away from the test's `HOME`.
        command.env_remove(OFF).env_remove("CI").env_remove("CURL_HOME");
        command.env_remove("XDG_CONFIG_HOME");
        command.env_remove("ATTICDB_TOKEN").env_remove("ATTICDB_SETUP_INTERACTIVE");
        command.envs(env.iter().copied()).stdin(Stdio::piped());
        Process::start(&mut command)
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
    // The home's server that setup starts may ask too, before either writes the cache.
    assert!(hits(&count) >= 1, "{out}{err}");
    assert_eq!(setup.read_cache()["latest"], "9.9.9");
    let last = out.lines().last().unwrap();
    match notice("9.9.9") {
        Some(line) => assert_eq!(last, line, "{out}"),
        None => assert!(last.starts_with("See what your agent gets at chat start: "), "{out}"),
    }
    assert!(!err.contains("curl") && !err.contains(" is out "), "{err}");
}

/// The hook and `mcp` never check, even with the check on, the API set and a stale cache:
/// the listener sees no connection. The home's server runs with the check off, so only the
/// hook and `mcp` could ask.
#[test]
fn hook_never_checks() {
    let setup = Setup::new();
    let data = setup.start_server();
    let _holders = Holders(vec![data, data_dir(&setup.dir.0.join("h"))]);
    let stale = json!({"checked": 1, "latest": "9.9.9"}).to_string();
    std::fs::write(setup.cache(), &stale).unwrap();
    let (url, count) = fake_api(200, TAG);
    let stdin = json!({"cwd": setup.dir.0}).to_string();
    for hook in ["session-start", "post-compact"] {
        let (code, out, err) = setup.run(&url, &["hook", hook], &[], &stdin);
        assert_eq!(code, Some(0), "{hook}: {out}{err}");
        assert_eq!(hits(&count), 0, "{hook}: {out}{err}");
        if hook == "session-start" && !atticdb::VERSION.ends_with("+dev") {
            assert!(out.contains("Update: AtticDB 9.9.9 is out"), "{out}");
        }
    }
    assert_eq!(std::fs::read_to_string(setup.cache()).unwrap(), stale);

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
    assert_eq!(std::fs::read_to_string(setup.cache()).unwrap(), stale);
}

/// Polls `done` every 50 ms for up to 20 s.
fn wait_for(mut done: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    done()
}

/// The home's server refreshes the cache once, in the background. With auto-upgrade on, a
/// release build installs the release. A source build never does: no `install.sh` is
/// fetched. It runs from a copy, so a stub never replaces the test's binary.
#[test]
fn home_server_refreshes_cache() {
    let setup = Setup::new();
    setup.make_home();
    std::fs::write(setup.home().join("auto-upgrade"), "on\n").unwrap();
    let release = Release::new("v9.9.9", stub(Stub::Install));
    let _holders = Holders(vec![setup.home().join("data")]);
    let exe = setup.copy_binary("bin2");
    let path = format!("{}:/bin:/usr/bin", setup.path.to_str().unwrap());
    let tmp = setup.tmp();
    let env = [("ATTICDB_DOWNLOAD_BASE", release.base.as_str()), ("TMPDIR", tmp.to_str().unwrap())];
    let args = ["fact", "set", "--key", "k", "--content", "v"];
    let (code, out, err) = setup.run_exe(&exe, path.as_ref(), &release.api, &args, &env, "");
    assert_eq!(code, Some(0), "{out}{err}");
    let cached = || std::fs::read(setup.cache()).ok().and_then(|t| serde_json::from_slice(&t).ok());
    let cached = wait_for(|| cached().is_some_and(|c: Value| c["latest"] == "9.9.9"));
    assert!(cached, "{:?}", release.paths());
    // Time for an auto-upgrade a release build would start.
    std::thread::sleep(std::time::Duration::from_secs(1));
    if atticdb::VERSION.ends_with("+dev") {
        assert_eq!(release.paths(), [LATEST.to_owned()]);
    } else {
        assert!(wait_for(|| std::fs::read(&exe).is_ok_and(|b| b == new_binary("9.9.9"))));
        assert_eq!(release.paths(), [LATEST.to_owned(), installer("9.9.9")]);
    }
}

/// A server the hook starts for its `--home` checks for that home, even when
/// `ATTICDB_HOME` names another. The hook passes the home on.
#[test]
fn hook_home_server_checks() {
    let setup = Setup::new();
    let _holders = Holders(vec![setup.home().join("data")]);
    let (url, count) = fake_api(200, TAG);
    let other = setup.dir.0.join("other");
    let home = setup.home();
    let stdin = json!({"cwd": setup.dir.0}).to_string();
    let args = ["hook", "session-start", "--home", home.to_str().unwrap()];
    let env = [("ATTICDB_HOME", other.to_str().unwrap())];
    let (code, out, err) = setup.run(&url, &args, &env, &stdin);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(home.join("data").exists(), "{out}{err}");
    assert!(wait_for(|| setup.cache().exists()), "{out}{err}");
    assert_eq!(setup.read_cache()["latest"], "9.9.9");
    assert!(hits(&count) >= 1);
    assert!(!other.exists());
}

/// Upgrades of 1 binary take turns on its folder's lock. One that waited finds the binary
/// another upgrade replaced, and changes nothing.
#[test]
fn upgrade_waits_its_turn() {
    let setup = Setup::new();
    setup.make_home();
    let exe = setup.copy_binary("bin2");
    let dir = exe.parent().unwrap().to_owned();
    let release = Release::new("v9.9.9", stub(Stub::Install));
    let folder = std::fs::File::open(&dir).unwrap();
    folder.lock().unwrap();
    let path = format!("{}:/bin:/usr/bin", setup.path.to_str().unwrap());
    let tmp = setup.tmp();
    let env = [("ATTICDB_DOWNLOAD_BASE", release.base.as_str()), ("TMPDIR", tmp.to_str().unwrap())];
    let mut process =
        setup.start_exe(&exe, path.as_ref(), &release.api, &["upgrade", "--yes"], &env);
    drop(process.child.stdin.take());
    assert!(wait_for(|| release.paths().contains(&LATEST.to_owned())));
    // Another upgrade replaces the binary while this one waits, or before it locks.
    let swap = dir.join(".swap");
    std::fs::write(&swap, "#!/bin/sh\necho 'atticdb 0.0.1'\n").unwrap();
    std::fs::set_permissions(&swap, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::rename(&swap, &exe).unwrap();
    drop(folder);
    let (status, _) = process.wait();
    let (out, err) = (process.stdout(), process.stderr());
    assert_eq!(status.code(), Some(0), "{out}{err}");
    let said =
        format!("Another upgrade changed {} meanwhile. It says: atticdb 0.0.1.", exe.display());
    assert!(out.contains(&said), "{out}");
    assert_eq!(release.paths(), [LATEST.to_owned()], "no install");
    assert_eq!(names(&dir), ["atticdb"]);
}

/// `upgrade --auto off` removes the switch. `--auto on` writes it, mode 0600, but never for a
/// source build. Doctor names it while it is on. Any other value, or another flag beside
/// `--auto`, is a usage error.
#[test]
fn upgrade_auto_switch() {
    let setup = Setup::new();
    setup.make_home();
    let marker = setup.home().join("auto-upgrade");
    let url = "http://127.0.0.1:9/";
    let log = setup.home().join("upgrade.log");

    std::fs::write(&marker, "on\n").unwrap();
    let (_, out, _) = setup.doctor_off(url);
    assert!(out.contains(&format!("Auto-upgrade: on. Log: {}\n", log.display())), "{out}");
    let (code, out, err) = setup.run(url, &["upgrade", "--auto", "off"], &[], "");
    assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "Auto-upgrade is off.\n", ""));
    assert!(!marker.exists());
    let (_, out, _) = setup.doctor_off(url);
    assert!(!out.contains("Auto-upgrade"), "{out}");

    let (code, out, err) = setup.run(url, &["upgrade", "--auto", "on"], &[], "");
    if atticdb::VERSION.ends_with("+dev") {
        assert_eq!(code, Some(1), "{out}{err}");
        assert!(err.ends_with("is a source build, which never auto-upgrades.\n"), "{err}");
        assert!(!marker.exists());
    } else {
        assert_eq!(code, Some(0), "{out}{err}");
        assert!(out.starts_with("Auto-upgrade is on."), "{out}");
        assert_eq!(std::fs::metadata(&marker).unwrap().mode() & 0o777, 0o600);
    }

    let (code, _, err) = setup.run(url, &["upgrade", "--auto", "maybe"], &[], "");
    assert_eq!((code, err.as_str()), (Some(2), "atticdb: --auto takes on or off\n"));
    let (code, out, err) = setup.run(url, &["upgrade", "--auto", "on", "--yes"], &[], "");
    assert_eq!(code, Some(2), "{out}{err}");
}

/// The warning every upgrade plan shows.
const WARNING: &str = "If this release changes the store format, the previous binary can't open \
                       the store after the first write.";
/// The warning instead of [`WARNING`] for an older release.
const DOWNGRADE: &str = "This is an older release. It may not open a store a newer release has \
                         written. Roll back with atticdb.prev. Releases before segment_mib \
                         refuse a home made by this release.";
/// What `upgrade` says when stdin is not a terminal and there is no `--yes`.
const NEEDS_TERMINAL: &str = "atticdb upgrade needs a terminal, or --yes.\n";

/// What the stub `install.sh` does.
#[derive(Clone, Copy, Debug)]
enum Stub {
    /// Writes the new binary as install.sh does: a temp file renamed over `atticdb`.
    Install,
    /// Fails before it writes anything, as on a checksum mismatch.
    Fail,
    /// Writes the new binary, then fails, as when the new one won't run.
    SwapThenFail,
}

/// The stub `install.sh`. It records its settings in `installer.env` beside the binary. The
/// new binary is a script that prints `new atticdb <version> <args>`.
fn stub(mode: Stub) -> String {
    let head = r#"#!/bin/sh
set -eu
dir=$ATTICDB_INSTALL_DIR
printf '%s\n' "ATTICDB_VERSION=$ATTICDB_VERSION" "ATTICDB_INSTALL_DIR=$dir" \
    "ATTICDB_DOWNLOAD_BASE=${ATTICDB_DOWNLOAD_BASE:-}" > "$dir/installer.env"
"#;
    let swap = r#"printf '#!/bin/sh\necho "new atticdb %s $*"\n' "$ATTICDB_VERSION" > "$dir/.part"
chmod 755 "$dir/.part"
mv -f "$dir/.part" "$dir/atticdb"
"#;
    let fail = "echo 'stub: checksum mismatch' >&2\nexit 1\n";
    match mode {
        Stub::Install => format!("{head}{swap}exit 0\n"),
        Stub::Fail => format!("{head}{fail}"),
        Stub::SwapThenFail => format!("{head}{swap}{fail}"),
    }
}

/// The bytes of the binary the stub installs at `version`.
fn new_binary(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"new atticdb {version} $*\"\n").into_bytes()
}

/// A fake release on 127.0.0.1: the API at `api` answers with `tag`, and any
/// `<base>/download/<tag>/install.sh` is `script`. `paths` lists the requests.
struct Release {
    base: String,
    api: String,
    paths: Arc<Mutex<Vec<String>>>,
}

impl Release {
    fn new(tag: &str, script: String) -> Self {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let (seen, tag) = (paths.clone(), json!({ "tag_name": tag }).to_string());
        let url = fake_http(move |path| {
            seen.lock().unwrap().push(path.to_owned());
            match path {
                "/repos/x/releases/latest" => (200, tag.clone()),
                path if path.starts_with("/releases/download/")
                    && path.ends_with("/install.sh") =>
                {
                    (200, script.clone())
                }
                _ => (404, String::new()),
            }
        });
        let api = format!("{url}/repos/x/releases/latest");
        Self { base: format!("{url}/releases"), api, paths }
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

/// The API's path, and the installer's for `version`.
const LATEST: &str = "/repos/x/releases/latest";
fn installer(version: &str) -> String {
    format!("/releases/download/v{version}/install.sh")
}

impl Setup {
    /// A copy of this test's `atticdb` at `<dir>/atticdb`, with `dir` made. Returns its real
    /// path, as `upgrade` prints it.
    fn copy_binary(&self, dir: &str) -> PathBuf {
        let dir = self.dir.0.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("atticdb");
        std::fs::copy(env!("CARGO_BIN_EXE_atticdb"), &exe).unwrap();
        std::fs::canonicalize(exe).unwrap()
    }

    /// `TMPDIR` for `upgrade`, so a test sees what it leaves there.
    fn tmp(&self) -> PathBuf {
        let tmp = self.dir.0.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    /// Runs `<exe> upgrade args` against `release`, with `env` added. `PATH` holds `curl` and
    /// the system's tools, for the stub.
    fn upgrade(
        &self,
        exe: &Path,
        release: &Release,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> Output {
        let path = format!("{}:/bin:/usr/bin", self.path.to_str().unwrap());
        let tmp = self.tmp();
        let mut all = vec![
            ("ATTICDB_DOWNLOAD_BASE", release.base.as_str()),
            ("TMPDIR", tmp.to_str().unwrap()),
        ];
        all.extend_from_slice(env);
        let args = [&["upgrade"], args].concat();
        self.run_exe(exe, path.as_ref(), &release.api, &args, &all, stdin)
    }

    /// Starts the home's server, by a write that makes the home. Returns the data dir.
    fn start_server(&self) -> PathBuf {
        let args = ["fact", "set", "--key", "k", "--content", "v"];
        let (code, out, err) = self.run("http://127.0.0.1:9/", &args, &[(OFF, "1")], "");
        assert_eq!(code, Some(0), "{out}{err}");
        let data = self.home().join("data");
        assert!(support::locked(&data), "{out}{err}");
        data
    }
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let entries = std::fs::read_dir(dir).unwrap();
    let mut names: Vec<String> =
        entries.map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort_unstable();
    names
}

/// The plan `upgrade` prints first, for `exe` going to `to`, a newer release.
fn plan(exe: &Path, to: &str) -> String {
    plan_with(exe, to, WARNING)
}

/// [`plan`] with `warning` as its last line.
fn plan_with(exe: &Path, to: &str, warning: &str) -> String {
    let exe = exe.display();
    let current = atticdb::VERSION;
    format!(
        "Upgrade AtticDB:\n  from: {current}\n  to: {to}\n  \
         notes: https://github.com/Clawdlinux/atticdb/releases/tag/v{to}\n  \
         binary: {exe}, the old one kept as {exe}.prev\n{warning}\n"
    )
}

/// `upgrade --yes` checks the API, then installs the release's `install.sh` pinned to it and
/// to the binary's folder. The old binary is kept as `atticdb.prev`. It ends with the new
/// binary's doctor. A check that failed within the hour doesn't stop it, and the temp file is
/// gone.
#[test]
fn upgrade_installs_and_keeps_prev() {
    let setup = Setup::new();
    setup.make_home();
    std::fs::write(setup.cache(), json!({"checked": now()}).to_string()).unwrap();
    let exe = setup.copy_binary("bin2");
    let old = std::fs::read(&exe).unwrap();
    // A `.prev` from an earlier upgrade gives way to this one's, with no `.prev.old` left.
    std::fs::write(exe.with_file_name("atticdb.prev"), "earlier rollback").unwrap();
    let release = Release::new("v9.9.9", stub(Stub::Install));
    let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(release.paths(), [LATEST.to_owned(), installer("9.9.9")]);
    assert!(out.starts_with(&plan(&exe, "9.9.9")), "{out}");
    let installed = format!("Installed AtticDB 9.9.9 at {}.\n", exe.display());
    assert!(out.contains(&installed), "{out}");
    assert!(out.ends_with("new atticdb 9.9.9 doctor\n"), "the new binary's doctor: {out}");
    assert!(!out.contains("Upgrade? [y/N]"), "{out}");

    assert_eq!(std::fs::read(&exe).unwrap(), new_binary("9.9.9"));
    let prev = exe.with_file_name("atticdb.prev");
    assert_eq!(std::fs::read(&prev).unwrap(), old, "the old bytes");
    assert_eq!(std::fs::metadata(&prev).unwrap().mode() & 0o777, 0o755);
    let dir = exe.parent().unwrap();
    let env = std::fs::read_to_string(dir.join("installer.env")).unwrap();
    let base = &release.base;
    let wanted = format!(
        "ATTICDB_VERSION=9.9.9\nATTICDB_INSTALL_DIR={}\nATTICDB_DOWNLOAD_BASE={base}\n",
        dir.display()
    );
    assert_eq!(env, wanted);
    assert_eq!(names(dir), ["atticdb", "atticdb.prev", "installer.env"]);
    assert!(names(&setup.tmp()).is_empty(), "the temp script is removed");
    assert_eq!(setup.read_cache()["latest"], "9.9.9");
}

/// `--version` wins: no check, and the installer is pinned to it. A `v` is dropped. A version
/// that isn't `x.y.z` is a usage error, with no request. So is a relative `ATTICDB_HOME`. An
/// older release gets the downgrade warning.
#[test]
fn upgrade_takes_version() {
    let setup = Setup::new();
    let exe = setup.copy_binary("bin2");
    let release = Release::new("v9.9.9", stub(Stub::Install));
    for bad in ["nightly", "1.2", "v1.2.3+x", "--yes", ""] {
        let (code, out, err) = setup.upgrade(&exe, &release, &["--yes", "--version", bad], &[], "");
        assert_eq!(code, Some(2), "{bad}: {out}{err}");
        assert!(err.starts_with("atticdb: --version "), "{bad}: {err}");
    }
    // upgrade reads the home, so a relative one is a usage error too.
    let rel = [("ATTICDB_HOME", "rel")];
    let (code, _, err) = setup.upgrade(&exe, &release, &["--yes"], &rel, "");
    assert_eq!((code, err.as_str()), (Some(2), "atticdb: ATTICDB_HOME must be an absolute path\n"));
    assert_eq!(release.paths(), Vec::<String>::new());

    let (code, out, err) =
        setup.upgrade(&exe, &release, &["--version", "v0.0.7", "--yes"], &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(release.paths(), [installer("0.0.7")], "no check");
    // 0.0.7 is older than this binary, so the plan warns about a downgrade instead.
    assert!(out.starts_with(&plan_with(&exe, "0.0.7", DOWNGRADE)), "{out}");
    assert!(!out.contains(WARNING), "{out}");
    assert_eq!(std::fs::read(&exe).unwrap(), new_binary("0.0.7"));
}

/// The upgrade stops the home's server by the pid in `LOCK`. That server must first prove it
/// is the one for this data dir. The lock is free when it returns. A `LOCK` held by something
/// that isn't a server is never signalled: here that is this test process.
#[test]
fn upgrade_stops_server() {
    let setup = Setup::new();
    let data = setup.start_server();
    let _holders = Holders(vec![data.clone()]);
    let lock = std::fs::read_to_string(data.join("LOCK")).unwrap();
    let pid = lock.strip_prefix("pid=").unwrap().split(' ').next().unwrap().to_owned();
    let exe = setup.copy_binary("bin2");
    let release = Release::new("v9.9.9", stub(Stub::Install));
    let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains(&format!("Stopped the server, pid {pid}.\n")), "{out}");
    assert!(!support::locked(&data), "the lock is free: {out}{err}");

    // A LOCK that names this process, held by it, with no server behind it.
    let me = std::process::id();
    let text = format!("pid={me} instance=0123 unix={}/no.sock tcp=none\n", data.display());
    let file = std::fs::File::options().write(true).truncate(true).open(data.join("LOCK"));
    let mut file = file.unwrap();
    file.write_all(text.as_bytes()).unwrap();
    file.lock().unwrap();
    let exe = setup.copy_binary("bin3");
    let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(err.contains("atticdb: the server was not stopped: "), "{err}");
    assert!(!out.contains("Stopped"), "{out}");
    assert!(support::locked(&data));
    drop(file);
}

/// An installer that fails leaves the binary as it was, also when it had replaced it. The
/// new `.prev` copy is removed, and the server keeps running. A `.prev` from an earlier
/// upgrade is kept, with no `.prev.old` left.
#[test]
fn upgrade_keeps_binary_when_installer_fails() {
    let setup = Setup::new();
    let data = setup.start_server();
    let _holders = Holders(vec![data.clone()]);
    let lock = std::fs::read_to_string(data.join("LOCK")).unwrap();
    let exe = setup.copy_binary("bin2");
    let old = std::fs::read(&exe).unwrap();
    let (dir, prev) = (exe.parent().unwrap(), exe.with_file_name("atticdb.prev"));
    let earlier: &[u8] = b"#!/bin/sh\necho the earlier rollback\n";
    for (mode, kept_prev) in [
        (Stub::Fail, false),
        (Stub::Fail, true),
        (Stub::SwapThenFail, false),
        (Stub::SwapThenFail, true),
    ] {
        let case = format!("{mode:?}, earlier .prev: {kept_prev}");
        // Read per case: a freed inode number can be reused by a later file on ext4.
        let ino = std::fs::metadata(&exe).unwrap().ino();
        if kept_prev {
            std::fs::write(&prev, earlier).unwrap();
        }
        let release = Release::new("v9.9.9", stub(mode));
        let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], &[], "");
        assert_eq!(code, Some(1), "{case}: {out}{err}");
        assert!(err.contains("stub: checksum mismatch\n"), "{case}: {err}");
        let kept = format!(
            "atticdb: the installer failed (exit status: 1). {} is unchanged.\n",
            exe.display()
        );
        assert!(err.ends_with(&kept), "{case}: {err}");
        assert!(!out.contains("Installed"), "{case}: {out}");
        assert_eq!(std::fs::read(&exe).unwrap(), old, "{case}");
        let now = std::fs::metadata(&exe).unwrap();
        // A swap is undone by renaming the copy back, so it is another file with the same
        // bytes and mode. Without a swap the binary is never touched.
        match mode {
            Stub::SwapThenFail => assert_ne!(now.ino(), ino, "{case}"),
            _ => assert_eq!(now.ino(), ino, "{case}"),
        }
        assert_eq!(now.mode() & 0o777, 0o755, "{case}");
        if kept_prev {
            assert_eq!(std::fs::read(&prev).unwrap(), earlier, "{case}");
            assert_eq!(names(dir), ["atticdb", "atticdb.prev", "installer.env"], "{case}");
            std::fs::remove_file(&prev).unwrap();
        } else {
            assert_eq!(names(dir), ["atticdb", "installer.env"], "{case}");
        }
        assert!(names(&setup.tmp()).is_empty(), "{case}");
        assert!(support::locked(&data), "{case}: the server runs on");
        assert_eq!(std::fs::read_to_string(data.join("LOCK")).unwrap(), lock, "{case}");
        std::fs::remove_file(dir.join("installer.env")).unwrap();
    }
}

/// Without a terminal on stdin and without `--yes`, it refuses with exit 2 before any request.
#[test]
fn upgrade_needs_yes_without_terminal() {
    let setup = Setup::new();
    let exe = setup.copy_binary("bin2");
    let old = std::fs::read(&exe).unwrap();
    let release = Release::new("v9.9.9", stub(Stub::Install));
    for args in [&[][..], &["--version", "9.9.9"]] {
        let (code, out, err) = setup.upgrade(&exe, &release, args, &[], "y\n");
        assert_eq!((code, out.as_str(), err.as_str()), (Some(2), "", NEEDS_TERMINAL), "{args:?}");
    }
    assert_eq!(release.paths(), Vec::<String>::new());
    assert_eq!(std::fs::read(&exe).unwrap(), old);
    assert_eq!(names(exe.parent().unwrap()), ["atticdb"]);
}

/// With a terminal, it shows the plan and asks. Only y or yes installs. No answer is exit 2.
#[test]
fn upgrade_asks_first() {
    let setup = Setup::new();
    let exe = setup.copy_binary("bin2");
    let old = std::fs::read(&exe).unwrap();
    let release = Release::new("v9.9.9", stub(Stub::Install));
    let terminal = [("ATTICDB_SETUP_INTERACTIVE", "1")];
    for answer in ["n\n", "\n", "maybe\n"] {
        let (code, out, err) = setup.upgrade(&exe, &release, &[], &terminal, answer);
        assert_eq!(code, Some(0), "{answer:?}: {out}{err}");
        let asked = format!("{}Upgrade? [y/N] Nothing changed.\n", plan(&exe, "9.9.9"));
        assert_eq!(out, asked, "{answer:?}");
    }
    let (code, out, err) = setup.upgrade(&exe, &release, &[], &terminal, "");
    assert_eq!(code, Some(2), "{out}{err}");
    assert_eq!(err, "atticdb: no answer on stdin. atticdb upgrade needs a terminal, or --yes.\n");
    assert_eq!(std::fs::read(&exe).unwrap(), old);
    assert_eq!(release.paths(), vec![LATEST.to_owned(); 4], "no installer yet");

    let (code, out, err) = setup.upgrade(&exe, &release, &[], &terminal, "Yes\n");
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("Upgrade? [y/N] Installed AtticDB 9.9.9 at "), "{out}");
    assert_eq!(std::fs::read(&exe).unwrap(), new_binary("9.9.9"));
}

/// When the latest release isn't newer, it says so and exits 0, with no installer. It asks
/// the API even when the cache holds a newer release from the last day.
#[test]
fn upgrade_up_to_date() {
    let setup = Setup::new();
    setup.make_home();
    let exe = setup.copy_binary("bin2");
    let current = atticdb::VERSION;
    // A source build counts as older than its own release, so only 0.0.0 is not newer.
    let latest: &[&str] = if current.ends_with("+dev") { &["0.0.0"] } else { &["0.0.0", current] };
    for latest in latest {
        std::fs::write(setup.cache(), json!({"checked": now(), "latest": "9.9.9"}).to_string())
            .unwrap();
        let release = Release::new(&format!("v{latest}"), stub(Stub::Install));
        let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], &[], "");
        assert_eq!((code, err.as_str()), (Some(0), ""), "{latest}: {out}");
        assert_eq!(out, format!("AtticDB {current} is the latest.\n"), "{latest}");
        assert_eq!(release.paths(), [LATEST], "{latest}");
        assert_eq!(setup.read_cache()["latest"], *latest);
    }
    assert_eq!(names(exe.parent().unwrap()), ["atticdb"]);
}

/// A failed check prints 1 error line and exits 1, with no installer. The off switches don't
/// apply, as the user asked to upgrade.
#[test]
fn upgrade_reports_a_failed_check() {
    let setup = Setup::new();
    let exe = setup.copy_binary("bin2");
    let release = Release::new("nightly", stub(Stub::Install));
    for env in [&[][..], &[(OFF, "1"), ("CI", "1")]] {
        let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], env, "");
        assert_eq!((code, out.as_str()), (Some(1), ""), "{env:?}: {err}");
        assert_eq!(err.lines().count(), 1, "{env:?}: {err}");
        assert!(err.starts_with("atticdb: cannot find the latest release"), "{env:?}: {err}");
    }
    assert_eq!(release.paths(), [LATEST, LATEST]);
    assert_eq!(names(exe.parent().unwrap()), ["atticdb"]);
}

/// A binary under `/opt/homebrew/` or a Homebrew `Cellar` gets its tool's command and exit 1,
/// as does a source build under `.cargo/bin`. Nothing runs: no request, and no change. A
/// symlink into the Cellar counts as the Cellar. A release under `.cargo/bin` came from
/// install.sh, so it upgrades.
#[test]
fn upgrade_points_cargo_and_brew_to_their_tool() {
    let setup = Setup::new();
    let cargo =
        "Installed by cargo. Run: cargo install --git https://github.com/Clawdlinux/atticdb \
                 atticdb --locked\n";
    let brew = "Installed by Homebrew. Run: brew upgrade atticdb\n";
    let cellar = setup.copy_binary("usr/local/Cellar/atticdb/0.1.0/bin");
    let link = setup.dir.0.join("usr/local/bin");
    std::fs::create_dir_all(&link).unwrap();
    std::os::unix::fs::symlink(&cellar, link.join("atticdb")).unwrap();
    let in_cargo = setup.copy_binary("h/.cargo/bin");
    let source = atticdb::VERSION.ends_with("+dev");
    let mut rows = vec![
        (setup.copy_binary("opt/homebrew/bin"), brew),
        (cellar.clone(), brew),
        (link.join("atticdb"), brew),
    ];
    if source {
        rows.push((in_cargo.clone(), cargo));
    }
    let release = Release::new("v9.9.9", stub(Stub::Install));
    for (exe, said) in rows {
        let old = std::fs::read(&exe).unwrap();
        let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], &[], "");
        assert_eq!((code, out.as_str(), err.as_str()), (Some(1), said, ""), "{}", exe.display());
        assert_eq!(std::fs::read(&exe).unwrap(), old);
        assert_eq!(names(exe.parent().unwrap()), ["atticdb"], "{}", exe.display());
    }
    assert_eq!(release.paths(), Vec::<String>::new());
    if !source {
        let (code, out, err) = setup.upgrade(&in_cargo, &release, &["--yes"], &[], "");
        assert_eq!(code, Some(0), "{out}{err}");
        assert_eq!(std::fs::read(&in_cargo).unwrap(), new_binary("9.9.9"));
        assert_eq!(release.paths(), [LATEST.to_owned(), installer("9.9.9")]);
    }
}

/// curl skips the user's `~/.curlrc`. One that caps downloads at 1 byte would fail the
/// doctor check, the upgrade's fresh check and the `install.sh` download.
#[test]
fn curl_ignores_curlrc() {
    let setup = Setup::new();
    setup.make_home();
    std::fs::write(setup.dir.0.join("h/.curlrc"), "max-filesize = 1\n").unwrap();
    let (url, count) = fake_api(200, TAG);
    let (_, out, err) = setup.run(&url, &["doctor"], &[], "");
    assert_eq!(hits(&count), 1, "{out}{err}");
    assert_eq!(setup.read_cache()["latest"], "9.9.9", "{out}{err}");

    let exe = setup.copy_binary("bin2");
    let release = Release::new("v9.9.9", stub(Stub::Install));
    let (code, out, err) = setup.upgrade(&exe, &release, &["--yes"], &[], "");
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(release.paths(), [LATEST.to_owned(), installer("9.9.9")]);
    assert_eq!(std::fs::read(&exe).unwrap(), new_binary("9.9.9"));
}
