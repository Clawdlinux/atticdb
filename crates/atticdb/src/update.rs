//! The daily check for a new release, and the notice `doctor` and `setup` print last. Those
//! verbs, `upgrade` and the home's `serve` check. The hook shows the notice from the cache
//! alone. The hook and `mcp` never ask the network. With auto-upgrade on, the home's `serve`
//! also runs `atticdb upgrade` for a newer release.

#[cfg(not(unix))]
use crate::platform::OpenOptionsExt;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Overrides the releases API's URL. Tests point it at a fake on 127.0.0.1.
pub(crate) const API_ENV: &str = "ATTICDB_RELEASES_API";
/// Turns the check off when set to a value other than empty or `0`.
pub(crate) const OFF_ENV: &str = "ATTICDB_NO_UPDATE_CHECK";
/// Set by CI services. Any value, even empty, turns the check off.
const CI_ENV: &str = "CI";
/// The latest release, by GitHub's API.
const DEFAULT_API: &str = "https://api.github.com/repos/Clawdlinux/atticdb/releases/latest";
/// The cache's name in the home: `{"checked": <unix seconds>, "latest": "<x.y.z>"}`. After a
/// failed check it holds `{"checked": <unix seconds>}` alone.
const CACHE: &str = "update-check.json";
/// How long the cache answers with a version.
const FRESH_SECS: u64 = 24 * 60 * 60;
/// How long after a failed check no other check runs.
const BACKOFF_SECS: u64 = 60 * 60;
/// Most bytes in a tag. A valid one has at most 30.
const MAX_TAG: usize = 32;
/// Most digits in a version's number.
const MAX_DIGITS: usize = 9;
/// How long curl may take, in seconds, as its `--max-time`.
const MAX_TIME: &str = "3";
/// Most bytes read from the API. GitHub's reply lists the assets and notes, so it gets room.
const MAX_REPLY: u64 = 1 << 20;
/// Most bytes of a download. `install.sh` is under 10 KiB.
#[cfg(unix)]
const MAX_DOWNLOAD: u64 = 1 << 20;
/// How long curl may take for a download, in seconds, as its `--max-time`.
#[cfg(unix)]
const DOWNLOAD_TIME: &str = "60";
/// Most bytes read from the cache, which is well under 100.
const MAX_CACHE: u64 = 4 << 10;
/// The file in the home that turns auto-upgrade on.
pub(crate) const AUTO: &str = "auto-upgrade";
/// The file in the home that auto-upgrades append their output to.
pub(crate) const UPGRADE_LOG: &str = "upgrade.log";
/// How often the home's server reads the cache. The cache still asks at most once a day.
const SERVE_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long the home's server waits before it tries the same release again.
const RETRY: Duration = Duration::from_secs(24 * 60 * 60);

/// The latest release's version, `x.y.z` without its `v`. The cache in `home` answers for
/// 24 h. Otherwise it asks the API, then writes the cache. A failed check is cached too, and
/// gives `None` for 1 h. `None` also when the off switches are set. It never prints, and
/// never makes `home`.
pub(crate) fn latest(home: &Path, now: SystemTime) -> Option<String> {
    let var = |name: &str| std::env::var_os(name);
    if off(var) {
        return None;
    }
    let api = std::env::var(API_ENV).ok().filter(|api| !api.is_empty());
    latest_from(home, now, api.as_deref().unwrap_or(DEFAULT_API))
}

/// Whether the off switches are set, as `var` reads them: [`OFF_ENV`] other than empty or
/// `0`, or [`CI_ENV`] set to anything.
fn off(var: impl Fn(&str) -> Option<OsString>) -> bool {
    let off = var(OFF_ENV).is_some_and(|value| !value.is_empty() && value != "0");
    off || var(CI_ENV).is_some()
}

/// [`latest`] with the API at `api`, whatever the off switches say.
fn latest_from(home: &Path, now: SystemTime, api: &str) -> Option<String> {
    let now = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    match cached(&home.join(CACHE), now) {
        Some(Cached::Latest(latest)) => return Some(latest),
        Some(Cached::Failed) => return None,
        None => {}
    }
    refresh(Some(home), now, api)
}

/// The latest release's version, asked now, as `upgrade` needs it. It skips the cache, the
/// 1 h back-off and the off switches, since the user asked. The answer still goes to the
/// cache in `home`, when there is one. `None` when the check fails.
#[cfg(unix)]
pub(crate) fn latest_fresh(home: Option<&Path>, now: SystemTime) -> Option<String> {
    let api = std::env::var(API_ENV).ok().filter(|api| !api.is_empty());
    let now = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    refresh(home, now, api.as_deref().unwrap_or(DEFAULT_API))
}

/// Asks the API at `api`, then caches the answer, or the failure, in `home` at `now`.
fn refresh(home: Option<&Path>, now: u64, api: &str) -> Option<String> {
    let latest = fetch(api);
    let entry = match &latest {
        Some(latest) => json!({"checked": now, "latest": latest}),
        None => json!({"checked": now}),
    };
    // A cache that can't be written costs a request next time, nothing more.
    if let Some(home) = home {
        let _ = store(home, &home.join(CACHE), &entry);
    }
    latest
}

/// What a cache entry that still counts says.
#[derive(Debug, PartialEq)]
enum Cached {
    /// The latest version, checked less than 24 h ago.
    Latest(String),
    /// A check that failed less than 1 h ago.
    Failed,
}

/// The cache entry, when it still counts at `now`. One from the future, after the clock was
/// set back, doesn't. Nor does one that can't be read, or holds a `latest` that isn't
/// `x.y.z`.
fn cached(cache: &Path, now: u64) -> Option<Cached> {
    let cache = read_cache(cache)?;
    let age = now.checked_sub(cache.get("checked")?.as_u64()?)?;
    let Some(latest) = cache.get("latest") else {
        return (age < BACKOFF_SECS).then_some(Cached::Failed);
    };
    let latest = latest.as_str()?;
    parse(latest)?;
    (age < FRESH_SECS).then(|| Cached::Latest(latest.to_owned()))
}

/// The cache file's JSON, or `None` when it is missing, too large or not JSON.
fn read_cache(cache: &Path) -> Option<Value> {
    let file = crate::config::open_regular(OpenOptions::new().read(true), cache).ok()?;
    let mut text = Vec::new();
    file.take(MAX_CACHE + 1).read_to_end(&mut text).ok()?;
    if text.len() as u64 > MAX_CACHE {
        return None;
    }
    serde_json::from_slice(&text).ok()
}

/// The `latest` version the cache in `home` holds, whatever its age. An old answer is still
/// a release that is out. It never asks the network. `None` when the off switches are set.
fn cached_latest(home: &Path) -> Option<String> {
    if off(|name| std::env::var_os(name)) {
        return None;
    }
    let latest = read_cache(&home.join(CACHE))?.get("latest")?.as_str()?.to_owned();
    parse(&latest).map(|_| latest)
}

/// The line the chat-start block shows when the cache in `home` names a newer release.
/// It reads the cache alone, so the hook stays offline.
pub(crate) fn hook_notice(home: &Path) -> Option<String> {
    let latest = cached_latest(home)?;
    let log = auto_on(home).then(|| home.join(UPGRADE_LOG));
    block_notice(crate::VERSION, &latest, log.as_deref(), cfg!(unix))
}

/// The block's line for `latest` over `current`, as [`notice`] decides. `log` is the
/// auto-upgrade log when auto-upgrade is on. `auto` says whether this platform has
/// auto-upgrade. Windows doesn't, and its `atticdb upgrade` prints the manual steps.
fn block_notice(current: &str, latest: &str, log: Option<&Path>, auto: bool) -> Option<String> {
    notice(current, latest)?;
    let (latest, current) = (bare(latest), bare(current));
    let head = format!("Update: AtticDB {latest} is out (you have {current}).");
    Some(match (log, auto) {
        (Some(log), true) => format!(
            "{head} Auto-upgrade is on, so the server installs it. If it doesn't, read {}.",
            log.display()
        ),
        (_, true) => format!(
            "{head} Tell the user once. To upgrade: atticdb upgrade. To upgrade on its own \
             from now on: atticdb upgrade --auto on."
        ),
        (_, false) => format!("{head} Tell the user once. To upgrade: atticdb upgrade."),
    })
}

/// Whether auto-upgrade is on: the home holds a regular file named [`AUTO`].
pub(crate) fn auto_on(home: &Path) -> bool {
    std::fs::symlink_metadata(home.join(AUTO)).is_ok_and(|meta| meta.is_file())
}

/// Whether an auto-upgrade from `current` to `latest` should run. Never for a source build.
fn auto_wanted(current: &str, latest: &str) -> bool {
    !current.ends_with("+dev") && newer(latest, current)
}

/// Starts the home server's update thread, when `data_dir` is the home's `data`. Other
/// servers, hosted or sandboxed, never check. Every [`SERVE_EVERY`] it calls [`latest`],
/// which asks at most once a day. With auto-upgrade on, a newer release starts
/// `atticdb upgrade` at most once per [`RETRY`] per version.
pub(crate) fn serve_watch(data_dir: &Path) {
    let Ok(home) = crate::home::dir() else { return };
    if data_dir != home.join("data") {
        return;
    }
    let _ = std::thread::Builder::new().name("atticdb-update".into()).spawn(move || {
        let mut tried: Option<(String, Instant)> = None;
        loop {
            if let Some(latest) = latest(&home, SystemTime::now()) {
                let recent = tried
                    .as_ref()
                    .is_some_and(|(version, at)| *version == latest && at.elapsed() < RETRY);
                if !recent && auto_on(&home) && auto_wanted(crate::VERSION, &latest) {
                    tried = Some((latest.clone(), Instant::now()));
                    auto_upgrade(&home, &latest);
                }
            }
            std::thread::sleep(SERVE_EVERY);
        }
    });
}

/// Runs `<this binary> upgrade --yes --version <latest>` in its own process group, so it
/// outlives this server, which it stops. Its output appends to [`UPGRADE_LOG`]. Says on
/// stderr, the server's log, whether it started.
#[cfg(unix)]
fn auto_upgrade(home: &Path, latest: &str) {
    match spawn_upgrade(home, latest) {
        Ok(pid) => eprintln!(
            "atticdb: auto-upgrade to {latest} started, pid {pid}, log {}",
            home.join(UPGRADE_LOG).display()
        ),
        Err(error) => eprintln!("atticdb: auto-upgrade to {latest} did not start: {error}"),
    }
}

/// Windows has no `atticdb upgrade`, so there is no auto-upgrade.
#[cfg(not(unix))]
fn auto_upgrade(_home: &Path, _latest: &str) {}

/// [`auto_upgrade`]'s spawn. Returns the child's pid. A thread reaps it.
#[cfg(unix)]
fn spawn_upgrade(home: &Path, latest: &str) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    let path = home.join(UPGRADE_LOG);
    let mut log = crate::config::open_regular(
        OpenOptions::new().create(true).append(true).mode(0o600),
        &path,
    )?;
    crate::config::check_private(&log, &path, "upgrade log").map_err(std::io::Error::other)?;
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    writeln!(log, "auto-upgrade at {secs} (unix seconds): {} to {latest}", crate::VERSION)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["upgrade", "--yes", "--version", latest]);
    command.env_remove(crate::client::TOKEN_ENV).current_dir("/").stdin(Stdio::null());
    command.stdout(log.try_clone()?).stderr(log).process_group(0);
    let mut child = crate::spawn(&mut command)?;
    let pid = child.id();
    let _ = std::thread::Builder::new().spawn(move || child.wait());
    Ok(pid)
}

/// Asks the API at `api` with `curl -q -fsSL --max-time 3`, and reads `tag_name`. `None` when
/// [`get`] fails, or for a tag that isn't `x.y.z` with an optional `v`.
fn fetch(api: &str) -> Option<String> {
    let reply = get(api, MAX_TIME, MAX_REPLY).ok()?;
    let reply: Value = serde_json::from_slice(&reply).ok()?;
    version_of(reply.get("tag_name")?.as_str()?)
}

/// Downloads `url` into `file`, which must not exist yet, with mode 0600. It uses [`get`]
/// with [`MAX_DOWNLOAD`] bytes in at most [`DOWNLOAD_TIME`] seconds. A file this made is
/// removed when the write fails.
#[cfg(unix)]
pub(crate) fn download(url: &str, file: &Path) -> Result<(), String> {
    let body = get(url, DOWNLOAD_TIME, MAX_DOWNLOAD)?;
    let mut options = OpenOptions::new();
    let opened = options.write(true).create_new(true).mode(0o600).open(file);
    let mut opened = opened.map_err(|error| format!("{}: {error}", file.display()))?;
    opened.write_all(&body).map_err(|error| {
        let _ = std::fs::remove_file(file);
        format!("{}: {error}", file.display())
    })
}

/// The body at `url`, by `curl -q -fsSL --max-time <max_time> <url>`, at most `max` bytes. It
/// fails for a `url` that isn't http or https. It also fails when curl is missing or fails,
/// or for a longer body.
fn get(url: &str, max_time: &str, max: u64) -> Result<Vec<u8>, String> {
    // Anything else could be a file URL, or read by curl as an option.
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("the URL is not http or https".into());
    }
    let mut command = Command::new(curl());
    command.args(curl_args(url, max_time));
    // curl needs no AtticDB token.
    command.env_remove(crate::client::TOKEN_ENV);
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    crate::platform::no_window(&mut command);
    let mut child = crate::spawn(&mut command).map_err(|error| format!("curl: {error}"))?;
    let mut body = Vec::new();
    let read = match child.stdout.take() {
        Some(stdout) => stdout.take(max + 1).read_to_end(&mut body).is_ok(),
        None => false,
    };
    let fits = body.len() as u64 <= max;
    if !read || !fits {
        let _ = child.kill();
    }
    let exited = child.wait().map_err(|error| format!("curl: {error}"))?;
    if !fits {
        return Err(format!("the reply is over {max} bytes"));
    }
    if !read || !exited.success() {
        return Err(format!("curl failed ({exited})"));
    }
    Ok(body)
}

/// curl's arguments for [`get`]. `-q` must come first, so curl skips the user's `.curlrc`.
/// A header or `user` line there would join the request.
fn curl_args<'a>(url: &'a str, max_time: &'a str) -> [&'a str; 5] {
    ["-q", "-fsSL", "--max-time", max_time, url]
}

/// The version in a release `tag`: `x.y.z` without the optional `v`. `None` for a tag over
/// [`MAX_TAG`] bytes, or a version [`parse`] rejects.
pub(crate) fn version_of(tag: &str) -> Option<String> {
    if tag.len() > MAX_TAG {
        return None;
    }
    let version = bare(tag);
    parse(version)?;
    Some(version.to_owned())
}

/// `curl` from `PATH`.
#[cfg(not(windows))]
fn curl() -> std::path::PathBuf {
    "curl".into()
}

/// `curl.exe` in the Windows folder, never one in the current folder or early on `PATH`.
#[cfg(windows)]
fn curl() -> std::path::PathBuf {
    crate::platform::system_program(r"System32\curl.exe")
}

/// Writes `entry`, mode 0600, to a temp file in `home` that it then renames over `cache`.
/// Fails when `home` is not a directory, so the home is never made here.
fn store(home: &Path, cache: &Path, entry: &Value) -> std::io::Result<()> {
    if !std::fs::metadata(home)?.is_dir() {
        return Err(std::io::Error::other("the home is not a directory"));
    }
    let text = entry.to_string();
    let hex = crate::random_hex(8)?;
    let tmp = home.join(format!(".{CACHE}.{hex}"));
    let mut options = OpenOptions::new();
    let written = options.write(true).create_new(true).mode(0o600).open(&tmp);
    let written = written.and_then(|mut file| file.write_all(text.as_bytes()));
    let written = written.and_then(|()| std::fs::rename(&tmp, cache));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The line to print when `latest` is newer than `current`, both `x.y.z` with an optional
/// `v`. `None` otherwise, and always for a source build, whose version ends in `+dev`.
pub(crate) fn notice(current: &str, latest: &str) -> Option<String> {
    if current.ends_with("+dev") || !newer(latest, current) {
        return None;
    }
    let (latest, current) = (bare(latest), bare(current));
    Some(format!("AtticDB {latest} is out (you have {current}). Run: atticdb upgrade"))
}

/// Whether `a` is newer than `b`, both `x.y.z` with an optional leading `v`, compared as
/// numbers. False when either is anything else.
pub(crate) fn newer(a: &str, b: &str) -> bool {
    matches!((parse(bare(a)), parse(bare(b))), (Some(a), Some(b)) if a > b)
}

/// `version` without its leading `v`, if any.
fn bare(version: &str) -> &str {
    version.strip_prefix('v').unwrap_or(version)
}

/// The 3 numbers of `x.y.z`. Each is 1 to [`MAX_DIGITS`] ASCII digits, with no leading zero
/// unless it is `0`. `None` for anything else, a `v` or a suffix such as `+dev` included.
fn parse(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        let part = parts.next().filter(|part| (1..=MAX_DIGITS).contains(&part.len()))?;
        let leading_zero = part.len() > 1 && part.starts_with('0');
        if leading_zero || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some(numbers)
}

/// Prints the notice when a newer release is out, by [`latest`] with the cache in `home`.
/// Prints nothing otherwise. Doctor and setup call it last.
pub(crate) fn show(home: &Path) {
    let Some(latest) = latest(home, SystemTime::now()) else { return };
    if let Some(line) = notice(crate::VERSION, &latest) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn newer_compares_numbers() {
        assert!(newer("0.3.0", "0.2.0"));
        assert!(!newer("0.2.0", "0.3.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(!newer("0.9.9", "0.10.0"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(newer("1.0.0", "v0.1.0"));
        assert!(!newer("1.2.3", "1.2.3"));
        assert!(!newer("v1.2.3", "1.2.3"));
        let garbage = [
            "",
            "v",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "1..3",
            "+1.2.3",
            "1.+2.3",
            "1.2.3+dev",
            "1.2.3-rc1",
            " 1.2.3",
            "vv1.2.3",
            "V1.2.3",
            "99999999999999999999.0.0",
            "1234567890.0.0",
            "01.2.3",
            "1.2.03",
            "1.00.3",
        ];
        for garbage in garbage {
            assert!(!newer(garbage, "0.0.1"), "{garbage:?}");
            assert!(!newer("9.9.9", garbage), "{garbage:?}");
        }
    }

    /// Every curl call goes through [`curl_args`], with `-q` first, so no `.curlrc` is read.
    #[test]
    fn curl_skips_curlrc() {
        let args = curl_args("https://example.com/x", "3");
        assert_eq!(args, ["-q", "-fsSL", "--max-time", "3", "https://example.com/x"]);
    }

    /// A tag is at most 32 bytes. Each number has at most 9 digits, and no leading zero
    /// unless it is `0`.
    #[test]
    fn tags_are_short_plain_versions() {
        assert_eq!(version_of("v1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(version_of("1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(version_of("0.10.0").as_deref(), Some("0.10.0"));
        assert_eq!(version_of("v0.0.0").as_deref(), Some("0.0.0"));
        let widest = "v999999999.999999999.999999999";
        assert_eq!(version_of(widest).as_deref(), Some(&widest[1..]));
        assert_eq!(version_of("01.2.3"), None);
        assert_eq!(version_of("v1.02.3"), None);
        assert_eq!(version_of("1.2.3333333333"), None);
        let long = format!("v1.2.3+{}", "a".repeat(26));
        assert_eq!(long.len(), 33);
        assert_eq!(version_of(&long), None);
        let long = format!("v1.2.{}", "0".repeat(28));
        assert_eq!(long.len(), 33);
        assert_eq!(version_of(&long), None);
    }

    #[test]
    fn block_notice_names_the_command() {
        let line = block_notice("0.3.3", "v0.3.4", None, true).unwrap();
        assert_eq!(
            line,
            "Update: AtticDB 0.3.4 is out (you have 0.3.3). Tell the user once. To upgrade: \
             atticdb upgrade. To upgrade on its own from now on: atticdb upgrade --auto on."
        );
        let log = Path::new("/h/upgrade.log");
        let line = block_notice("0.3.3", "0.3.4", Some(log), true).unwrap();
        assert_eq!(
            line,
            "Update: AtticDB 0.3.4 is out (you have 0.3.3). Auto-upgrade is on, so the server \
             installs it. If it doesn't, read /h/upgrade.log."
        );
        // Windows has no auto-upgrade, so its line names only the manual command.
        let windows = "Update: AtticDB 0.3.4 is out (you have 0.3.3). Tell the user once. To \
                       upgrade: atticdb upgrade.";
        assert_eq!(block_notice("0.3.3", "0.3.4", None, false).as_deref(), Some(windows));
        assert_eq!(block_notice("0.3.3", "0.3.4", Some(log), false).as_deref(), Some(windows));
        assert_eq!(block_notice("0.3.4", "0.3.4", None, true), None);
        assert_eq!(block_notice("0.3.3+dev", "0.3.4", None, true), None);
    }

    #[test]
    fn auto_only_for_a_newer_release() {
        assert!(auto_wanted("0.3.3", "0.3.4"));
        assert!(!auto_wanted("0.3.4", "0.3.4"));
        assert!(!auto_wanted("0.3.4", "0.3.3"));
        assert!(!auto_wanted("0.3.3+dev", "0.3.4"));
    }

    #[test]
    fn cached_latest_ignores_age() {
        let dir =
            std::env::temp_dir().join(format!("atticdb-cl-{}", crate::random_hex(8).unwrap()));
        std::fs::create_dir(&dir).unwrap();
        let cases = [
            (r#"{"checked": 1, "latest": "0.3.4"}"#, Some("0.3.4")),
            (r#"{"checked": 1}"#, None),
            (r#"{"checked": 1, "latest": "v0.3.4"}"#, None),
            ("not json", None),
        ];
        for (text, want) in cases {
            std::fs::write(dir.join(CACHE), text).unwrap();
            // Unless the off switches are set in this test's environment.
            let want = want.filter(|_| !off(|name| std::env::var_os(name)));
            assert_eq!(cached_latest(&dir).as_deref(), want, "{text}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn notice_only_for_a_newer_release() {
        let line = "AtticDB 0.3.0 is out (you have 0.2.0). Run: atticdb upgrade";
        assert_eq!(notice("0.2.0", "0.3.0").as_deref(), Some(line));
        assert_eq!(notice("v0.2.0", "v0.3.0").as_deref(), Some(line));
        assert_eq!(notice("0.3.0", "0.3.0"), None);
        assert_eq!(notice("0.3.0", "0.2.0"), None);
        assert_eq!(notice("0.2.0", "nightly"), None);
        assert_eq!(notice("nightly", "0.3.0"), None);
        // A source build never shows it, whatever the release.
        assert_eq!(notice("0.2.0+dev", "9.9.9"), None);
        assert_eq!(notice(concat!(env!("CARGO_PKG_VERSION"), "+dev"), "9.9.9"), None);
    }

    #[test]
    fn off_switches() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.into())
        };
        assert!(!off(env(&[])));
        assert!(off(env(&[("ATTICDB_NO_UPDATE_CHECK", "1")])));
        assert!(off(env(&[("ATTICDB_NO_UPDATE_CHECK", "true")])));
        assert!(!off(env(&[("ATTICDB_NO_UPDATE_CHECK", "")])));
        assert!(!off(env(&[("ATTICDB_NO_UPDATE_CHECK", "0")])));
        assert!(off(env(&[("CI", "true")])));
        assert!(off(env(&[("CI", "")])));
    }

    /// These run the real `curl` against a fake API on 127.0.0.1.
    #[cfg(unix)]
    mod check {
        use super::super::*;
        use serde_json::{json, Value};
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, UNIX_EPOCH};

        const DAY: u64 = 24 * 60 * 60;
        const T: u64 = 1_800_000_000;
        const TAG: &str = r#"{"tag_name": "v9.9.9", "name": "AtticDB 9.9.9"}"#;

        /// A private temp dir for a home, removed on drop.
        struct Home(PathBuf);

        impl Home {
            fn new() -> Self {
                static NEXT: AtomicUsize = AtomicUsize::new(0);
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                let name = format!("atticdb-update-{}-{n}", std::process::id());
                let dir = std::env::temp_dir().join(name);
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
                Self(dir)
            }

            fn cache(&self) -> PathBuf {
                self.0.join("update-check.json")
            }

            fn names(&self) -> Vec<String> {
                let entries = std::fs::read_dir(&self.0).unwrap();
                let mut names: Vec<String> =
                    entries.map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
                names.sort_unstable();
                names
            }
        }

        impl Drop for Home {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// A fake releases API on 127.0.0.1 that answers each request with `status` and
        /// `body`. Returns its URL and a count of connections.
        fn api(status: u16, body: String) -> (String, Arc<AtomicUsize>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
            let hits = Arc::new(AtomicUsize::new(0));
            let counted = hits.clone();
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
                        "HTTP/1.1 {status} X\r\nContent-Length: {length}\r\n\
                         Connection: close\r\n\r\n{body}"
                    );
                    let _ = stream.write_all(reply.as_bytes());
                }
            });
            (url, hits)
        }

        fn at(secs: u64) -> SystemTime {
            UNIX_EPOCH + Duration::from_secs(secs)
        }

        fn hits(count: &AtomicUsize) -> usize {
            count.load(Ordering::SeqCst)
        }

        fn cache(home: &Home) -> Value {
            serde_json::from_slice(&std::fs::read(home.cache()).unwrap()).unwrap()
        }

        /// The first check asks the API and writes the cache, mode 0600, through a temp file
        /// it renames. For 24 h the cache answers. Then, or with a clock set back before the
        /// check, it asks again.
        #[test]
        fn checks_then_caches_for_a_day() {
            let home = Home::new();
            let (url, count) = api(200, TAG.into());
            assert_eq!(latest_from(&home.0, at(T), &url).as_deref(), Some("9.9.9"));
            assert_eq!(hits(&count), 1);
            assert_eq!(cache(&home), json!({"checked": T, "latest": "9.9.9"}));
            assert_eq!(std::fs::metadata(home.cache()).unwrap().mode() & 0o777, 0o600);
            assert_eq!(home.names(), ["update-check.json"], "no temp file left");

            let fresh = latest_from(&home.0, at(T + DAY - 1), &url);
            assert_eq!(fresh.as_deref(), Some("9.9.9"));
            assert_eq!(hits(&count), 1, "the cache answers for 24 h");

            let (url, count) = api(200, r#"{"tag_name": "v10.0.0"}"#.into());
            let stale = latest_from(&home.0, at(T + DAY), &url);
            assert_eq!(stale.as_deref(), Some("10.0.0"));
            assert_eq!(hits(&count), 1, "a day-old cache is checked again");
            assert_eq!(cache(&home), json!({"checked": T + DAY, "latest": "10.0.0"}));

            let back = latest_from(&home.0, at(T), &url);
            assert_eq!(back.as_deref(), Some("10.0.0"));
            assert_eq!(hits(&count), 2, "a clock set back checks again");
        }

        /// Any failure gives `None` and caches `{"checked": <now>}` alone. Failures are an
        /// HTTP error, bad JSON or no `tag_name`. So are a bad or long tag, an oversized
        /// reply or no server.
        #[test]
        fn failures_give_none_and_cache_the_time() {
            let big = format!(r#"{{"tag_name": "v9.9.9", "pad": "{}"}}"#, "x".repeat(2 << 20));
            let rows = [
                (500, TAG.to_owned()),
                (404, TAG.to_owned()),
                (200, "{".to_owned()),
                (200, "[]".to_owned()),
                (200, r#"{"name": "v9.9.9"}"#.to_owned()),
                (200, r#"{"tag_name": 9}"#.to_owned()),
                (200, r#"{"tag_name": "nightly"}"#.to_owned()),
                (200, r#"{"tag_name": "vv9.9.9"}"#.to_owned()),
                (200, r#"{"tag_name": "v9.9.9\u001b[2J"}"#.to_owned()),
                (200, r#"{"tag_name": "v09.9.9"}"#.to_owned()),
                (200, r#"{"tag_name": "v1.2.3333333333"}"#.to_owned()),
                (200, big),
            ];
            for (status, body) in rows {
                let home = Home::new();
                let (url, count) = api(status, body.clone());
                assert_eq!(latest_from(&home.0, at(T), &url), None, "{status} {body:.60}");
                assert_eq!(hits(&count), 1, "{status} {body:.60}");
                assert_eq!(cache(&home), json!({"checked": T}), "{status} {body:.60}");
                assert_eq!(home.names(), ["update-check.json"], "{status} {body:.60}");
            }
            let home = Home::new();
            let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let refused = format!("http://127.0.0.1:{port}/releases/latest");
            assert_eq!(latest_from(&home.0, at(T), &refused), None);
            assert_eq!(cache(&home), json!({"checked": T}));
        }

        /// After a failed check, none runs for 1 h. Then, or with a clock set back before the
        /// failure, it checks again. A version replaces the failure.
        #[test]
        fn failed_check_backs_off_for_an_hour() {
            let home = Home::new();
            let (url, count) = api(500, TAG.into());
            assert_eq!(latest_from(&home.0, at(T), &url), None);
            assert_eq!(hits(&count), 1);
            let (url, count) = api(200, TAG.into());
            assert_eq!(latest_from(&home.0, at(T + 3_599), &url), None);
            assert_eq!(hits(&count), 0, "no check within the hour");
            assert_eq!(latest_from(&home.0, at(T - 1), &url).as_deref(), Some("9.9.9"));
            assert_eq!(hits(&count), 1, "a clock set back checks again");

            std::fs::write(home.cache(), json!({"checked": T}).to_string()).unwrap();
            assert_eq!(latest_from(&home.0, at(T + 3_600), &url).as_deref(), Some("9.9.9"));
            assert_eq!(hits(&count), 2, "an hour later it checks again");
            assert_eq!(cache(&home), json!({"checked": T + 3_600, "latest": "9.9.9"}));
        }

        /// Only an http or https URL goes to curl, so the URL can't be read as an option.
        /// Any other counts as a failed check.
        #[test]
        fn only_http_urls() {
            let (url, count) = api(200, TAG.into());
            let options = url.replace("http://", "-o/dev/null http://");
            for api in ["file:///etc/hosts", "-K/etc/hosts", "", &options] {
                let home = Home::new();
                assert_eq!(latest_from(&home.0, at(T), api), None, "{api}");
                assert_eq!(cache(&home), json!({"checked": T}), "{api}");
            }
            assert_eq!(hits(&count), 0);
        }

        /// A cache that can't be read as `{"checked": <secs>, "latest": "x.y.z"}` is checked
        /// again and replaced.
        #[test]
        fn bad_cache_is_checked_again() {
            let rows = [
                String::new(),
                "{".into(),
                "{}".into(),
                json!({"latest": "1.2.3"}).to_string(),
                json!({"checked": T.to_string(), "latest": "1.2.3"}).to_string(),
                json!({"checked": T.to_string()}).to_string(),
                json!({"checked": T, "latest": null}).to_string(),
                json!({"checked": T, "latest": "01.2.3"}).to_string(),
                json!({"checked": T, "latest": "nightly"}).to_string(),
                json!({"checked": T, "latest": 1}).to_string(),
            ];
            for text in rows {
                let home = Home::new();
                std::fs::write(home.cache(), &text).unwrap();
                let (url, count) = api(200, TAG.into());
                let found = latest_from(&home.0, at(T + 1), &url);
                assert_eq!(found.as_deref(), Some("9.9.9"), "{text}");
                assert_eq!(hits(&count), 1, "{text}");
                assert_eq!(cache(&home), json!({"checked": T + 1, "latest": "9.9.9"}), "{text}");
            }
        }

        /// A missing home is never made. The check still answers.
        #[test]
        fn missing_home_is_not_made() {
            let home = Home::new();
            let missing = home.0.join("missing");
            let (url, count) = api(200, TAG.into());
            assert_eq!(latest_from(&missing, at(T), &url).as_deref(), Some("9.9.9"));
            assert_eq!(hits(&count), 1);
            assert!(!missing.exists());
        }
    }
}
