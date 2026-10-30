//! Names from before AtticDB was renamed from zerodb, in 1 place. The guards stop AtticDB
//! from making a new, empty home beside zerodb data. A light read finds zerodb parts in each
//! app. `atticdb migrate` moves an existing install: the home, then each app's parts.

use crate::connect::{self, Harness, Notes, Options};
use crate::home::{Lookup, CONFIG, HOME_ENV};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The old name: the binary, the default home's directory and the MCP server key.
const NAME: &str = "zerodb";
/// The variable that moved the old home.
const OLD_HOME_ENV: &str = "ZERODB_HOME";
/// Each renamed variable, with its new name.
const RENAMED_ENV: [(&str, &str); 2] =
    [(OLD_HOME_ENV, HOME_ENV), ("ZERODB_TOKEN", crate::mcp::TOKEN_ENV)];
/// The old home's config file.
const OLD_CONFIG: &str = "zerodb.json";
/// The old server's socket in `data/`.
const OLD_SOCKET: &str = "zerodb.sock";
/// The line the old binary put in the Copilot instructions file it wrote.
const OLD_MARKER: &str = "<!-- written by zerodb connect -->";
/// The file `migrate` writes in the old home just before moving it, holding the old path. It
/// moves with the home, so a rerun after a crash knows to leave the note. It stays.
const MARKER: &str = "MOVED_FROM_ZERODB";
/// How the note left at the old default home starts.
const MOVED: &str = "AtticDB moved this home to";
/// Where the README says how to move.
const HOW: &str = "https://github.com/Clawdlinux/atticdb#moving-from-zerodb";
/// The next step once nothing needs the old binary.
const UNINSTALL: &str = "Stop using the zerodb binary: `cargo uninstall zerodb`.";
/// How long the old binary's `disconnect` may take.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Most bytes read from an app's file.
const MAX_READ: u64 = 1 << 20;

/// Refuses an old variable that is set while its new name is not.
pub fn check_env() -> Result<(), String> {
    match RENAMED_ENV.into_iter().find(|(old, new)| set(old) && !set(new)) {
        Some((old, new)) => Err(format!("{old} was renamed to {new}. Rename it and run again.")),
        None => Ok(()),
    }
}

/// Removes the old variables from a child's environment, so its guards pass.
pub(crate) fn clear_env(command: &mut Command) {
    for (old, _) in RENAMED_ENV {
        command.env_remove(old);
    }
}

/// Whether the variable `name` is set and not empty.
fn set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Refuses the home `dir` when zerodb data is in its way: `dir` holds the old config and not
/// the new one, or, for the `default` home, `dir` is missing and the old default home is a
/// directory. Then AtticDB would make a new, empty home beside the old data.
pub(crate) fn check_home(dir: &Path, default: bool) -> Result<(), String> {
    if exists(&dir.join(OLD_CONFIG)) && !exists(&dir.join(CONFIG)) {
        return Err(found(dir));
    }
    if default && !exists(dir) {
        let old = crate::home::default_dir(NAME)?;
        if old.is_dir() {
            return Err(found(&old));
        }
    }
    Ok(())
}

fn found(path: &Path) -> String {
    format!(
        "Found zerodb data at {}. AtticDB was called zerodb. Run `atticdb migrate` to move it. \
         Nothing changed.",
        path.display()
    )
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// The checks of [`crate::home::dir`] for the home command `command`, before it runs. An old
/// variable stops every one but doctor. zerodb data in the home's way stops every one but
/// doctor and setup. Doctor reports both, and setup's plan shows the home's as a stop line. A
/// home that can't be found, such as with `HOME` unset, is left to the command.
pub fn guard(command: &str) -> Result<(), String> {
    if command == "doctor" {
        return Ok(());
    }
    check_env()?;
    match crate::home::unchecked_dir() {
        Ok((dir, default)) if command != "setup" => check_home(&dir, default),
        _ => Ok(()),
    }
}

/// zerodb parts in 1 app's files, found by a light read.
#[derive(Debug, Default)]
struct Parts {
    /// Each file with parts in it, and what to remove there by hand.
    files: Vec<(PathBuf, String)>,
    /// The binaries the entries and hooks run, as written.
    binaries: Vec<PathBuf>,
    /// Whether a SessionStart hook runs zerodb. Claude Code and Codex only.
    hook: bool,
}

impl Parts {
    fn add(&mut self, file: &Path, what: impl Into<String>) {
        self.files.push((file.to_owned(), what.into()));
    }

    /// The MCP entry keyed `zerodb` in the JSON file at `path`, under `section`.
    fn entry(&mut self, path: &Path, section: &str) {
        let Some(text) = read(path) else { return };
        if !has_key(&text) {
            return;
        }
        self.add(path, format!("the \"{NAME}\" entry in \"{section}\""));
        let root: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let command = root[section][NAME]["command"].as_str().map(PathBuf::from);
        self.binaries.extend(command);
    }

    /// The SessionStart hooks in the JSON file at `path` that run a binary named zerodb.
    fn hooks(&mut self, path: &Path) {
        let found = read(path).map(|text| hook_binaries(&text)).unwrap_or_default();
        if !found.is_empty() {
            self.add(path, format!("the SessionStart hooks that run {NAME}"));
            self.binaries.extend(found);
            self.hook = true;
        }
    }
}

/// The text of the regular file at `path`, up to [`MAX_READ`] bytes. `None` when it is
/// missing or can't be read.
fn read(path: &Path) -> Option<String> {
    let file = crate::config::open_regular(OpenOptions::new().read(true), path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_READ).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether `text` has the JSON key `"zerodb"`.
fn has_key(text: &str) -> bool {
    let quoted = format!("\"{NAME}\"");
    text.match_indices(&quoted)
        .any(|(at, _)| text[at + quoted.len()..].trim_start().starts_with(':'))
}

/// The binaries of the hook commands in `text` that run ` hook session-start` with a binary
/// named zerodb, single-quoted or bare.
fn hook_binaries(text: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices(" hook session-start") {
        let before = &text[..at];
        let word = match before.strip_suffix('\'') {
            Some(quoted) => quoted.rsplit_once('\'').map(|(_, word)| word),
            None => before.rsplit(|c: char| c.is_whitespace() || c == '"').next(),
        };
        let exe = word.map(PathBuf::from).filter(|exe| exe.file_name() == Some(NAME.as_ref()));
        found.extend(exe);
    }
    found
}

/// The parts in `harness`'s own files. The Copilot files VS Code and Copilot CLI share are
/// in [`shared`].
fn parts(harness: Harness) -> Parts {
    let mut parts = Parts::default();
    match harness {
        Harness::Vscode | Harness::Copilot => {
            if let Ok(file) = harness.file() {
                parts.entry(&file, harness.section());
            }
        }
        Harness::ClaudeCode => {
            if let Ok(state) = connect::claude_state() {
                parts.entry(&state, harness.section());
            }
            if let Ok(settings) = connect::claude_settings() {
                if read(&settings).is_some_and(|text| text.contains("mcp__zerodb__")) {
                    parts.add(&settings, "the mcp__zerodb__ allow rules");
                }
                parts.hooks(&settings);
            }
        }
        Harness::Codex => {
            if let Ok(config) = connect::codex_home().map(|dir| dir.join("config.toml")) {
                codex_config(&mut parts, &config);
            }
            if let Ok(hooks) = connect::codex_hooks() {
                parts.hooks(&hooks);
            }
            for guide in ["AGENTS.md", "AGENTS.override.md"] {
                let Ok(path) = connect::codex_home().map(|dir| dir.join(guide)) else { continue };
                let text = read(&path).unwrap_or_default();
                if text.lines().any(|line| line.trim().starts_with("<!-- zerodb:begin")) {
                    parts.add(&path, "the <!-- zerodb:begin --> to <!-- zerodb:end --> block");
                }
            }
        }
    }
    parts
}

/// The `# zerodb:begin` block or `[mcp_servers.zerodb]` tables in Codex's `config.toml`, and
/// the `command` of that server.
fn codex_config(parts: &mut Parts, path: &Path) {
    let Some(text) = read(path) else { return };
    let (mut found, mut inside) = (false, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with("# zerodb:begin") {
            found = true;
        }
        if line.starts_with('[') {
            inside = line == "[mcp_servers.zerodb]";
            found |= inside;
        }
        let value = line.strip_prefix("command").map(str::trim_start);
        if let Some(value) = value.and_then(|rest| rest.strip_prefix('=')).filter(|_| inside) {
            let exe = serde_json::from_str::<String>(value.trim()).ok();
            parts.binaries.extend(exe.map(PathBuf::from));
        }
    }
    if found {
        parts.add(path, "the # zerodb:begin to # zerodb:end block, and [mcp_servers.zerodb]");
    }
}

/// The parts in the Copilot files VS Code and Copilot CLI share: the hook file, and the
/// instructions file the old binary wrote.
fn shared() -> Parts {
    let mut parts = Parts::default();
    let Ok(dir) = connect::copilot_home() else { return parts };
    let hook = dir.join("hooks/zerodb.json");
    if exists(&hook) {
        parts.add(&hook, "the file");
        parts.binaries.extend(read(&hook).map(|text| hook_binaries(&text)).unwrap_or_default());
    }
    let guide = dir.join("instructions/zerodb.instructions.md");
    if read(&guide).is_some_and(|text| text.lines().any(|line| line.trim_end() == OLD_MARKER)) {
        parts.add(&guide, "the file");
    }
    parts
}

/// Each app with zerodb parts, in the order `migrate` disconnects them: VS Code, Copilot CLI,
/// Claude Code, then Codex. The shared Copilot files count for each of VS Code and Copilot
/// CLI that has an entry, else for Copilot CLI.
fn scan() -> Vec<(Harness, Parts)> {
    let mut apps: Vec<(Harness, Parts)> = Harness::ALL.map(|h| (h, parts(h))).into();
    let shared = shared();
    if !shared.files.is_empty() {
        let copilot = |h: &Harness| matches!(h, Harness::Vscode | Harness::Copilot);
        let entries = apps.iter().any(|(h, parts)| copilot(h) && !parts.files.is_empty());
        for (harness, parts) in apps.iter_mut().filter(|(h, _)| copilot(h)) {
            if !parts.files.is_empty() || (!entries && *harness == Harness::Copilot) {
                parts.files.extend(shared.files.iter().cloned());
                parts.binaries.extend(shared.binaries.iter().cloned());
            }
        }
    }
    apps.retain(|(_, parts)| !parts.files.is_empty());
    apps
}

/// A line for each file of `harness` with zerodb parts, for setup's plan and doctor.
pub(crate) fn found_lines(harness: Harness) -> Vec<String> {
    let apps = scan().into_iter().filter(|(h, _)| *h == harness);
    let mut files: Vec<PathBuf> = apps.flat_map(|(_, parts)| parts.files).map(|(f, _)| f).collect();
    files.dedup();
    files
        .iter()
        .map(|f| format!("zerodb parts found in {}; run `atticdb migrate`", f.display()))
        .collect()
}

/// What `migrate` found at the old home.
enum Old {
    /// Nothing, or something that isn't a home.
    Missing,
    /// The note a finished move left at the old default home.
    Moved,
    /// A home directory.
    Home,
}

fn old_kind(path: &Path) -> Old {
    if path.is_dir() {
        Old::Home
    } else if read(path).is_some_and(|text| text.starts_with(MOVED)) {
        Old::Moved
    } else {
        Old::Missing
    }
}

/// Prints `would` in a plan, else does `act` and prints `did`.
fn step(
    plan: bool,
    would: String,
    did: String,
    act: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if plan {
        println!("Would {would}.");
    } else {
        act()?;
        println!("{did}.");
    }
    Ok(())
}

/// `atticdb migrate`, or with `plan` what it would do, writing nothing. Returns the exit
/// code: 0 when everything is done or there is nothing to do, 1 when a step stopped, 2 for a
/// usage error.
pub fn migrate(plan: bool) -> u8 {
    match run(plan) {
        Ok(code) => code,
        Err((code, error)) => {
            eprintln!("atticdb: {error}");
            code
        }
    }
}

fn run(plan: bool) -> Result<u8, (u8, String)> {
    let stop = |error: String| (1, error);
    let old_env = std::env::var_os(OLD_HOME_ENV).filter(|dir| !dir.is_empty()).map(PathBuf::from);
    if old_env.as_ref().is_some_and(|dir| !dir.is_absolute()) {
        return Err((2, format!("{OLD_HOME_ENV} must be an absolute path")));
    }
    let old = crate::home::default_dir(NAME).map_err(stop)?;
    // The home AtticDB will read: `ATTICDB_HOME` when set, else the default. The old default
    // home moves there, so nothing is left behind an empty one.
    let env_home = crate::home::env_dir().map_err(|error| (2, error))?;
    let new = match &env_home {
        Some(dir) => dir.clone(),
        None => crate::home::default_dir(crate::home::NAME).map_err(stop)?,
    };
    // Setup reads that home, or the one ZERODB_HOME names, without the guards, as the old
    // variables may still be set here.
    let lookup = Lookup::given(env_home.or_else(|| old_env.clone()));
    let mut acted = false;
    let mut moved = false;
    match &old_env {
        Some(dir) if dir.is_dir() => {
            running(dir).map_err(stop)?;
            finish(dir, false, plan).map_err(stop)?;
            println!("The zerodb home at {} stays where it is.", dir.display());
            println!("Set {HOME_ENV}={}.", dir.display());
            acted = true;
        }
        Some(dir) => println!("No zerodb home found at {} ({OLD_HOME_ENV}).", dir.display()),
        None => match old_kind(&old) {
            Old::Moved => {
                println!(
                    "The zerodb home at {} was already moved to {}.",
                    old.display(),
                    new.display()
                );
                acted |= finish(&new, true, plan).map_err(stop)?;
            }
            Old::Home => {
                running(&old).map_err(stop)?;
                if exists(&new) {
                    return Err(stop(format!(
                        "Both homes exist: {} and {}. Merge them by hand. Nothing changed.",
                        old.display(),
                        new.display()
                    )));
                }
                let (from, to) = (old.display(), new.display());
                step(plan, format!("move {from} to {to}"), format!("Moved {from} to {to}"), || {
                    mark(&old)?;
                    move_home(&old, &new)
                })
                .map_err(stop)?;
                // A plan names the files where they are now.
                finish(if plan { &old } else { &new }, true, plan).map_err(stop)?;
                (acted, moved) = (true, true);
            }
            Old::Missing => {
                // A run that stopped after the move left its marker, and maybe the old
                // config, in the new home, and no note at the old path.
                moved = !exists(&old) && marked(&new, &old);
                if finish(&new, true, plan).map_err(stop)? || moved {
                    acted = true;
                } else {
                    println!("No zerodb home found.");
                }
            }
        },
    }
    let apps = scan();
    let mut failed = false;
    for (harness, parts) in &apps {
        failed |= !disconnect(*harness, parts, old_env.as_deref(), plan);
    }
    warn_instructions();
    if moved {
        let line = format!("{MOVED} {}. See {HOW}\n", new.display());
        let (would, did) = (
            format!("leave a note at {} that says so", old.display()),
            format!("Left a note at {}", old.display()),
        );
        step(plan, would, did, || tombstone(&old, &line)).map_err(stop)?;
    }
    if !acted && apps.is_empty() {
        println!("Nothing to migrate.");
        return Ok(0);
    }
    if plan {
        if !apps.is_empty() {
            let names: Vec<&str> = apps.iter().map(|(h, _)| h.name()).collect();
            println!("Would connect {} again, as atticdb setup --yes does.", names.join(", "));
        }
        println!("Dry run: nothing was written.");
        return Ok(0);
    }
    if apps.is_empty() {
        println!("Next:\n  {UNINSTALL}\n  Start new chat sessions.");
        return Ok(u8::from(failed));
    }
    let order = crate::setup::ORDER.into_iter();
    let chosen: Vec<(Harness, Options)> = order
        .filter_map(|h| apps.iter().find(|(app, _)| *app == h))
        .map(|(h, parts)| {
            let hook = parts.hook || matches!(h, Harness::Vscode | Harness::Copilot);
            (*h, Options { force: false, hook, notes: Notes::Auto })
        })
        .collect();
    let code = crate::setup::apply(&chosen, vec![UNINSTALL.into()], &lookup);
    Ok(code.max(u8::from(failed)))
}

/// Refuses when a server holds the lock on the old home's data directory.
fn running(dir: &Path) -> Result<(), String> {
    let Some(holder) = crate::serve::lock_holder(&dir.join("data")) else { return Ok(()) };
    let pid = holder.strip_prefix("pid=").and_then(|rest| rest.split(' ').next());
    let pid = pid.filter(|pid| pid.parse::<u32>().is_ok()).unwrap_or("unknown");
    Err(format!(
        "A zerodb server is running (pid {pid}). Quit the apps that use zerodb, then stop it: \
         `kill {pid}`. Nothing changed."
    ))
}

/// Writes [`MARKER`] in the old home `dir`, holding its path, and syncs it.
fn mark(dir: &Path) -> Result<(), String> {
    let path = dir.join(MARKER);
    let fail = |error: std::io::Error| {
        format!("{}: cannot write: {error}. Nothing changed.", path.display())
    };
    let mut options = OpenOptions::new();
    let mut file =
        options.write(true).create(true).truncate(true).mode(0o600).open(&path).map_err(fail)?;
    file.write_all(format!("{}\n", dir.display()).as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(fail)?;
    File::open(dir).and_then(|dir| dir.sync_all()).map_err(fail)
}

/// Whether the home `dir` holds [`MARKER`] naming `old`, so it was moved from there.
fn marked(dir: &Path, old: &Path) -> bool {
    read(&dir.join(MARKER)).is_some_and(|text| text.trim_end() == old.display().to_string())
}

/// Renames the old default home to the new one, making the new one's parent at mode 0700 if
/// it is missing.
fn move_home(old: &Path, new: &Path) -> Result<(), String> {
    if let Some(parent) = new.parent().filter(|parent| !exists(parent)) {
        let mut builder = std::fs::DirBuilder::new();
        let made = builder.recursive(true).mode(0o700).create(parent);
        made.map_err(|error| {
            format!("{}: cannot create: {error}. Nothing changed.", parent.display())
        })?;
    }
    match std::fs::rename(old, new) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::CrossesDevices => Err(format!(
            "{} and {} are on different filesystems; move it by hand. Nothing changed.",
            old.display(),
            new.display()
        )),
        Err(error) => Err(format!(
            "cannot move {} to {}: {error}. Nothing changed.",
            old.display(),
            new.display()
        )),
    }
}

/// Renames the old config in the home `dir` to the new name, when only the old one is there.
/// With `socket`, also removes a stale old socket in `data/`. Then syncs, as `ensure` does.
/// Returns whether anything needed doing. A running server stops it first.
fn finish(dir: &Path, socket: bool, plan: bool) -> Result<bool, String> {
    let (old, new) = (dir.join(OLD_CONFIG), dir.join(CONFIG));
    let stale = dir.join("data").join(OLD_SOCKET);
    let rename = exists(&old) && !exists(&new);
    let remove = socket && exists(&stale);
    if rename || remove {
        running(dir)?;
    }
    if rename {
        let (from, to) = (old.display(), new.display());
        step(
            plan,
            format!("rename {from} to {CONFIG}"),
            format!("Renamed {from} to {CONFIG}"),
            || {
                std::fs::rename(&old, &new)
                    .map_err(|error| format!("cannot rename {from} to {to}: {error}"))
            },
        )?;
    }
    if remove {
        let shown = stale.display();
        step(
            plan,
            format!("remove the stale socket {shown}"),
            format!("Removed the stale socket {shown}"),
            || {
                std::fs::remove_file(&stale)
                    .map_err(|error| format!("cannot remove {shown}: {error}"))
            },
        )?;
    }
    if (rename || remove) && !plan {
        crate::home::sync(dir)?;
    }
    Ok(rename || remove)
}

/// Writes `line` to a new mode 0600 file at the old default home `path`, so a zerodb binary
/// still running there fails loudly instead of making a new, empty home.
fn tombstone(path: &Path, line: &str) -> Result<(), String> {
    let fail =
        |error: std::io::Error| format!("{}: cannot write the note: {error}", path.display());
    let mut file =
        OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(fail)?;
    file.write_all(line.as_bytes()).and_then(|()| file.sync_all()).map_err(fail)?;
    let parent = path.parent().unwrap_or(Path::new("/"));
    File::open(parent).and_then(|dir| dir.sync_all()).map_err(fail)
}

/// Runs the old binary's own `disconnect` for `harness`, which removes exactly what it added,
/// or prints what to remove by hand when there is no old binary. Returns whether the parts
/// are gone, or would be.
fn disconnect(harness: Harness, parts: &Parts, old_env: Option<&Path>, plan: bool) -> bool {
    let named = parts.binaries.iter().find(|exe| exe.is_absolute() && exe.is_file()).cloned();
    let Some(exe) = named.or_else(|| connect::find_program(NAME)) else {
        let app = crate::setup::title(harness);
        println!("No zerodb binary found to disconnect {app}. Remove these by hand:");
        for (file, what) in &parts.files {
            println!("  {}: {what}", file.display());
        }
        return false;
    };
    let line = format!("{} disconnect {}", exe.display(), harness.name());
    if plan {
        println!("Would run {line}.");
        return true;
    }
    println!("Ran {line}:");
    let mut command = Command::new(&exe);
    command.args(["disconnect", harness.name()]).stdin(Stdio::null());
    if let Some(dir) = old_env {
        command.env(OLD_HOME_ENV, dir);
    }
    match output(&mut command) {
        Ok((ok, lines)) => {
            for line in lines {
                println!("  {line}");
            }
            ok
        }
        Err(why) => {
            println!("  {why}");
            false
        }
    }
}

/// Runs `command` for up to [`DISCONNECT_TIMEOUT`]. Returns whether it exited 0, and its
/// stdout then stderr lines.
fn output(command: &mut Command) -> Result<(bool, Vec<String>), String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = crate::spawn(command).map_err(|error| format!("cannot start it: {error}"))?;
    let collect = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stream.map(|stream| stream.take(MAX_READ).read_to_string(&mut text));
            text
        })
    };
    let out = collect(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err = collect(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let deadline = Instant::now() + DISCONNECT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "it took over {} s, so it was stopped",
                    DISCONNECT_TIMEOUT.as_secs()
                ));
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let mut lines: Vec<String> = Vec::new();
    for reader in [out, err] {
        lines.extend(reader.join().unwrap_or_default().lines().map(str::to_owned));
    }
    if !status.success() {
        lines.push(format!("it failed ({status})"));
    }
    Ok((status.success(), lines))
}

/// Warns about each instructions file in Copilot's folder that mentions zerodb and that the
/// old binary didn't write. It never edits them.
fn warn_instructions() {
    let Ok(dir) = connect::copilot_home().map(|dir| dir.join("instructions")) else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut paths: Vec<PathBuf> = entries.map_while(Result::ok).map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let named = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        let Some(text) = read(&path).filter(|_| named.ends_with(".instructions.md")) else {
            continue;
        };
        let old = text.lines().any(|line| line.trim_end() == OLD_MARKER);
        if !old && text.to_lowercase().contains(NAME) {
            println!("{} mentions zerodb. Update it by hand.", path.display());
        }
    }
}
