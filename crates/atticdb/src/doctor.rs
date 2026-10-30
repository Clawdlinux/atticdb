//! `atticdb doctor`: checks this install and writes nothing. Each check prints 1 line, `ok` or
//! the problem and what to do. The store check may start the home's server, as any read does.
//! The parts check is [`connect::connect`] in [`Mode::Plan`], the code that writes.

use crate::connect::{self, Harness, Mode, Notes, Options, Report};
use crate::setup::{check_hook, check_store, title, ORDER};
use atticdb_tape::{Io, StdIo};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// What `atticdb doctor --help` prints.
pub const USAGE: &str = "usage: atticdb doctor [--harness <list>]

Checks this atticdb install and writes nothing: the binary, the home, the server and store,
and each app. The store check may start the home's server.

  --harness <list>  check these apps, a comma list of copilot, vscode, claude-code and codex

Exit 0 when every check is ok or only warns, 1 when a check failed, 2 for a usage error.";

/// What doctor says about a home that is missing, or lacks its config or data dir.
const NOT_SET_UP: &str = "not set up yet, run atticdb setup";

/// The options a Claude Code or Codex install may have been connected with, the defaults
/// first. Parts are in place when any of them plans no change.
const CHOSEN: [Options; 4] = [
    Options { force: false, hook: true, notes: Notes::Auto },
    Options { force: false, hook: true, notes: Notes::Ask },
    Options { force: false, hook: false, notes: Notes::Auto },
    Options { force: false, hook: false, notes: Notes::Ask },
];

/// The checks' results so far.
#[derive(Default)]
struct Checks {
    /// Whether a check failed.
    failed: bool,
}

impl Checks {
    /// Prints `  <what>: ok`, or `  <what>: <why>` and counts a failure.
    fn line(&mut self, what: &str, result: Result<(), String>) {
        match result {
            Ok(()) => println!("  {what}: ok"),
            Err(why) => {
                println!("  {what}: {why}");
                self.failed = true;
            }
        }
    }

    /// Prints `  <text>` and counts a failure.
    fn fail(&mut self, text: &str) {
        println!("  {text}");
        self.failed = true;
    }

    /// Prints `  <what>: warning: <why>`. A warning is not a failure.
    fn warn(&self, what: &str, why: &str) {
        println!("  {what}: warning: {why}");
    }
}

/// Runs `atticdb doctor` on `harnesses`, else on all 4 apps. Returns the exit code: 1 when a
/// check failed, else 0.
pub fn run(harnesses: Option<Vec<Harness>>) -> u8 {
    let mut checks = Checks::default();
    println!("atticdb:");
    let (version, exe) = (env!("CARGO_PKG_VERSION"), connect::binary());
    match &exe {
        Ok(exe) => checks.line(&format!("version {version}, {exe}"), Ok(())),
        Err(why) => checks.line(&format!("version {version}"), Err(why.clone())),
    }
    println!("Home:");
    let dir = match crate::home::dir() {
        Ok(dir) => dir,
        Err(why) => {
            checks.line("home", Err(why));
            sandboxes(None);
            return u8::from(checks.failed);
        }
    };
    let set = match crate::home::env_dir() {
        Ok(Some(_)) => "set by ATTICDB_HOME",
        _ => "ATTICDB_HOME is not set",
    };
    let what = format!("{} ({set})", dir.display());
    let missing = |path: &Path| matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == ErrorKind::NotFound);
    if missing(&dir) {
        checks.line(&what, Err(NOT_SET_UP.into()));
        sandboxes(Some(&dir));
        return 1;
    }
    // A home without its config or data dir isn't set up. Reading it would make them, with a
    // new token, so the checks that read stop here. So they do for a symlink in their place,
    // which a read may not follow as doctor does.
    let (mut partial, mut odd) = (false, Vec::new());
    for (name, kind) in [("atticdb.json", "regular file"), ("data", "directory")] {
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => partial = true,
            Ok(meta) if meta.is_file() && name == "atticdb.json" => {}
            Ok(meta) if meta.is_dir() && name == "data" => {}
            _ => {
                odd.push(format!("{} is a symlink or not a {kind}; fix it by hand", path.display()))
            }
        }
    }
    let checked = crate::home::check_existing();
    let home_ok = checked.is_ok() && odd.is_empty();
    match checked {
        Ok(()) if !odd.is_empty() => checks.line(&what, Err(odd.join("; "))),
        Ok(()) if partial => checks.line(&what, Err(NOT_SET_UP.into())),
        checked => checks.line(&what, checked),
    }
    disk(&mut checks, &dir);
    if !home_ok || partial {
        let first = if home_ok { "set it up" } else { "fix the home" };
        println!("Server and store: skipped, {first} first");
        sandboxes(Some(&dir));
        return 1;
    }
    let cwd = std::env::current_dir().map_err(|error| format!("no current dir: {error}"));
    match cwd.clone().and_then(|cwd| check_store(&cwd)) {
        Ok(()) => println!("Server and store: ok"),
        Err(why) => {
            println!("Server and store: {why}");
            checks.failed = true;
        }
    }
    for harness in harnesses.unwrap_or_else(|| ORDER.to_vec()) {
        app(&mut checks, harness, &exe, &cwd);
    }
    sandboxes(Some(&dir));
    u8::from(checks.failed)
}

/// Checks `harness`, when it is found: its parts, the binary its entry and hook run, and
/// the hook. `exe` is this binary.
fn app(
    checks: &mut Checks,
    harness: Harness,
    exe: &Result<String, String>,
    cwd: &Result<PathBuf, String>,
) {
    if !connect::detect(harness) {
        println!("{}: not found", title(harness));
        return;
    }
    println!("{}:", title(harness));
    let plan = plan(harness);
    let name = harness.name();
    let fix = format!("run atticdb setup --harness {name}, or atticdb connect {name}");
    let changes: Vec<&str> = (plan.lines.iter())
        .filter(|line| !line.state.in_place())
        .map(|line| line.text.as_str())
        .collect();
    match &plan.stopped {
        Some((_, error)) => checks.line("parts", Err(error.clone())),
        None if changes.is_empty() => checks.line("parts", Ok(())),
        None => {
            checks.line("parts", Err(format!("{} would change, {fix}", changes.len())));
            for change in changes {
                println!("    {change}");
            }
        }
    }
    for found in crate::legacy::found_lines(harness) {
        checks.fail(&found);
    }
    let stale = match exe {
        Ok(exe) => stale(harness, exe),
        Err(why) => vec![why.clone()],
    };
    if stale.is_empty() {
        checks.line("binary", Ok(()));
    } else {
        checks.line("binary", Err(format!("stale, {fix}")));
        for stale in stale {
            println!("    {stale}");
        }
    }
    for warning in &plan.warnings {
        println!("  note: {warning}");
    }
    if plan.hooks_off {
        checks.warn("hook", HOOKS_OFF);
    } else if plan.hook.is_some_and(connect::State::in_place) {
        checks.line("hook", cwd.clone().and_then(|cwd| check_hook(harness, &cwd)));
    }
    if harness == Harness::Codex {
        // It only reads config, and never shows codex's output, which may hold secrets.
        let what = "codex mcp get atticdb";
        if connect::on_path("codex") {
            checks.line(what, connect::verify_codex());
        } else {
            println!("  {what}: not verified: codex not found");
        }
    }
}

/// What doctor says when Codex's config turns its hooks off.
const HOOKS_OFF: &str = "Codex's hooks are off, so memory won't load at chat start. Turn them \
                         on in [features] in config.toml: remove hooks = false, or set hooks = true";

/// The plan for `harness`. For Claude Code and Codex, the first of [`CHOSEN`] that changes
/// nothing, else the defaults' plan. A hook counts as left out on purpose only when it is
/// missing, so a hook that would change is never hidden.
fn plan(harness: Harness) -> Report {
    let in_place = |report: &Report| {
        report.stopped.is_none() && report.lines.iter().all(|line| line.state.in_place())
    };
    let first = connect::connect(harness, CHOSEN[0], Mode::Plan);
    if in_place(&first) || !matches!(harness, Harness::ClaudeCode | Harness::Codex) {
        return first;
    }
    let hookless = first.hook == Some(connect::State::Added);
    for options in CHOSEN[1..].iter().filter(|options| options.hook || hookless) {
        let report = connect::connect(harness, *options, Mode::Plan);
        if in_place(&report) {
            return report;
        }
    }
    first
}

/// A line for each AtticDB binary that `harness`'s entry or hook runs and that is not `exe`, or
/// no longer exists.
fn stale(harness: Harness, exe: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (file, named) in connect::binaries(harness) {
        let file = file.display();
        // A link to this binary runs it too.
        match std::fs::canonicalize(&named) {
            Err(_) => found.push(format!("{file} runs {named}, which no longer exists")),
            Ok(real) if real != Path::new(exe) => {
                found.push(format!("{file} runs {named}, not this atticdb, {exe}"));
            }
            Ok(_) => {}
        }
    }
    found
}

/// Checks the disk under the home `dir`: free space against the store's floor, the kind of
/// filesystem, and whether a sync app copies it.
fn disk(checks: &mut Checks, dir: &Path) {
    let data = dir.join("data");
    let on = if data.exists() { data } else { dir.to_owned() };
    let io = StdIo::new();
    match io.available_bytes(&on).and_then(|free| Ok((free, io.total_bytes(&on)?))) {
        Ok((free, total)) => {
            let floor = atticdb_core::EngineOptions::default().tape.free_space_floor;
            let floor = floor.unwrap_or_else(|| atticdb_tape::default_free_space_floor(total));
            let what = format!("free space: {} free, the floor is {}", size(free), size(floor));
            match space(free, floor) {
                Space::Ok => checks.line(&what, Ok(())),
                Space::Low => checks.warn(
                    &what,
                    "under 2x the floor. Writes stop below the floor, so free some space soon",
                ),
                Space::Full => checks.line(
                    &what,
                    Err("under the floor, so writes stop. Free some space on this disk".into()),
                ),
            }
        }
        Err(error) => checks.line("free space", Err(format!("{}: {error}", on.display()))),
    }
    match fs_type(&on) {
        Some(name) if remote(&name) => checks.warn(
            &format!("filesystem: {name}"),
            "a network or FUSE filesystem, where locks and syncs may not hold. Keep the home \
             on a local disk, with ATTICDB_HOME",
        ),
        Some(name) => checks.line(&format!("filesystem: {name}"), Ok(())),
        None => checks.line("filesystem: unknown", Ok(())),
    }
    let user = crate::home::user_home().ok();
    let real = std::fs::canonicalize(&on).ok();
    let found = [Some(dir.to_owned()), real].into_iter().flatten();
    match found.filter_map(|path| synced(&path, user.as_deref())).next() {
        Some(app) => checks.warn(
            "synced folder",
            &format!(
                "the home is in {app}, which may copy the store while it is written. Keep the \
                 home outside synced folders, with ATTICDB_HOME"
            ),
        ),
        None => checks.line("synced folder", Ok(())),
    }
}

/// Free space against the floor.
#[derive(Debug, PartialEq, Eq)]
enum Space {
    /// At least 2x the floor.
    Ok,
    /// Under 2x the floor.
    Low,
    /// Under the floor: writes stop.
    Full,
}

/// How `free` bytes compare to `floor`.
fn space(free: u64, floor: u64) -> Space {
    if free < floor {
        Space::Full
    } else if free < floor.saturating_mul(2) {
        Space::Low
    } else {
        Space::Ok
    }
}

/// `bytes` in GiB, MiB or bytes.
fn size(bytes: u64) -> String {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    match bytes {
        GIB.. => format!("{:.1} GiB", bytes as f64 / GIB as f64),
        MIB.. => format!("{:.1} MiB", bytes as f64 / MIB as f64),
        _ => format!("{bytes} bytes"),
    }
}

/// The type of the filesystem that holds `path`, from `statfs`: `f_fstypename` on macOS, and
/// the `f_type` magic number on Linux. `None` when it can't be read.
#[cfg(target_os = "macos")]
fn fs_type(path: &Path) -> Option<String> {
    let stat = rustix::fs::statfs(path).ok()?;
    // `c_char` is `i8` here, so each byte is cast back.
    let name: Vec<u8> =
        stat.f_fstypename.iter().take_while(|byte| **byte != 0).map(|byte| *byte as u8).collect();
    String::from_utf8(name).ok()
}

/// See the macOS version.
#[cfg(target_os = "linux")]
fn fs_type(path: &Path) -> Option<String> {
    let stat = rustix::fs::statfs(path).ok()?;
    // `f_type`'s type differs between targets. Every one fits in an `i128`.
    Some(match i128::from(stat.f_type) {
        0x6969 => "nfs".into(),
        0x517b => "smb".into(),
        0xff53_4d42 => "cifs".into(),
        0xfe53_4d42 => "smb2".into(),
        0x6573_5546 => "fuse".into(),
        magic => format!("type {magic:#x}"),
    })
}

/// See the macOS version.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn fs_type(_path: &Path) -> Option<String> {
    None
}

/// Whether the filesystem type `name` is a network or FUSE one: NFS, SMB or CIFS, or FUSE.
fn remote(name: &str) -> bool {
    ["nfs", "smb", "cifs"].iter().any(|kind| name.starts_with(kind)) || name.contains("fuse")
}

/// The sync app that copies `path`, if any: iCloud Drive, a folder in `~/Library/CloudStorage`
/// such as Dropbox, OneDrive or Google Drive on macOS, or a path through a `Dropbox`,
/// `OneDrive` or `Google Drive` folder. `user` is `$HOME`.
fn synced(path: &Path, user: Option<&Path>) -> Option<&'static str> {
    if let Some(user) = user {
        if path.starts_with(user.join("Library/Mobile Documents")) {
            return Some("iCloud Drive");
        }
        if path.starts_with(user.join("Library/CloudStorage")) {
            return Some("a cloud storage folder");
        }
    }
    let text = format!("{}/", path.display());
    let apps =
        [("/Dropbox/", "Dropbox"), ("/OneDrive/", "OneDrive"), ("/Google Drive/", "Google Drive")];
    apps.into_iter().find(|(folder, _)| text.contains(folder)).map(|(_, app)| app)
}

/// Prints the sandbox note. Doctor runs outside any sandbox, so it can't check one.
fn sandboxes(dir: Option<&PathBuf>) {
    let dir = dir.map_or_else(|| "the atticdb home".into(), |dir| dir.display().to_string());
    println!("Sandboxes:\n  Agents in a sandbox need read and write access to {dir}.");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Free space against a 100-byte floor, and filesystem names.
    #[test]
    fn space_and_filesystems() {
        for (free, wanted) in [(0, Space::Full), (99, Space::Full), (100, Space::Low)] {
            assert_eq!(space(free, 100), wanted, "{free}");
        }
        for (free, wanted) in [(199, Space::Low), (200, Space::Ok), (u64::MAX, Space::Ok)] {
            assert_eq!(space(free, 100), wanted, "{free}");
        }
        for name in ["nfs", "smbfs", "cifs", "smb2", "fuse", "macfuse", "osxfuse"] {
            assert!(remote(name), "{name}");
        }
        for name in ["apfs", "hfs", "ext4", "btrfs", "type 0xef53"] {
            assert!(!remote(name), "{name}");
        }
    }
}
