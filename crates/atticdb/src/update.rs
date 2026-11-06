//! The daily check for a new release, and the notice `doctor` and `setup` print last. Only
//! those verbs and `upgrade` check. The hook, `mcp` and `serve` never do.

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
use std::time::{SystemTime, UNIX_EPOCH};

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
/// Most bytes read from the cache, which is well under 100.
const MAX_CACHE: u64 = 4 << 10;

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
    let cache = home.join(CACHE);
    match cached(&cache, now) {
        Some(Cached::Latest(latest)) => return Some(latest),
        Some(Cached::Failed) => return None,
        None => {}
    }
    let latest = fetch(api);
    let entry = match &latest {
        Some(latest) => json!({"checked": now, "latest": latest}),
        None => json!({"checked": now}),
    };
    // A cache that can't be written costs a request next time, nothing more.
    let _ = store(home, &cache, &entry);
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
    let file = crate::config::open_regular(OpenOptions::new().read(true), cache).ok()?;
    let mut text = Vec::new();
    file.take(MAX_CACHE + 1).read_to_end(&mut text).ok()?;
    if text.len() as u64 > MAX_CACHE {
        return None;
    }
    let cache: Value = serde_json::from_slice(&text).ok()?;
    let age = now.checked_sub(cache.get("checked")?.as_u64()?)?;
    let Some(latest) = cache.get("latest") else {
        return (age < BACKOFF_SECS).then_some(Cached::Failed);
    };
    let latest = latest.as_str()?;
    parse(latest)?;
    (age < FRESH_SECS).then(|| Cached::Latest(latest.to_owned()))
}

/// Asks the API at `api` with `curl -fsSL --max-time 3`, and reads `tag_name`. `None` for an
/// `api` that isn't http or https. Also `None` when curl is missing or fails, or the reply is
/// over [`MAX_REPLY`]. So is a tag that isn't `x.y.z` with an optional `v`.
fn fetch(api: &str) -> Option<String> {
    // Anything else could be a file URL, or read by curl as an option.
    if !api.starts_with("https://") && !api.starts_with("http://") {
        return None;
    }
    let mut command = Command::new(curl());
    command.args(["-fsSL", "--max-time", MAX_TIME, api]);
    // curl needs no AtticDB token.
    command.env_remove(crate::client::TOKEN_ENV);
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    crate::platform::no_window(&mut command);
    let mut child = crate::spawn(&mut command).ok()?;
    let mut reply = Vec::new();
    let read = match child.stdout.take() {
        Some(stdout) => stdout.take(MAX_REPLY + 1).read_to_end(&mut reply).is_ok(),
        None => false,
    };
    let read = read && reply.len() as u64 <= MAX_REPLY;
    if !read {
        let _ = child.kill();
    }
    let exited = child.wait().ok()?;
    if !read || !exited.success() {
        return None;
    }
    let reply: Value = serde_json::from_slice(&reply).ok()?;
    version_of(reply.get("tag_name")?.as_str()?)
}

/// The version in a release `tag`: `x.y.z` without the optional `v`. `None` for a tag over
/// [`MAX_TAG`] bytes, or a version [`parse`] rejects.
fn version_of(tag: &str) -> Option<String> {
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
