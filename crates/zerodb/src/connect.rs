//! `zerodb connect` and `zerodb disconnect`: add or remove zerodb's entry in a harness's
//! user MCP config, and the Copilot `sessionStart` hook file both harnesses share. The
//! entry and the hook run the binary on the home, so they hold no token. The only paths
//! they hold are the binary's and, under `ZERODB_HOME`, the home's.

use crate::home::{user_home, HOME_ENV};
use serde_json::{json, Map, Value};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zerodb_tape::{Io, StdIo};

/// A harness whose user MCP config zerodb can edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    /// VS Code's `mcp.json`, used by its Copilot chat.
    Vscode,
    /// Copilot CLI's `mcp-config.json`.
    Copilot,
}

impl Harness {
    /// Every harness.
    pub const ALL: [Self; 2] = [Self::Vscode, Self::Copilot];

    /// The harness called `name` on the command line.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "vscode" => Some(Self::Vscode),
            "copilot" => Some(Self::Copilot),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Vscode => "vscode",
            Self::Copilot => "copilot",
        }
    }

    /// The `--agent` its entry passes, so turns are kept per harness.
    pub fn agent(self) -> &'static str {
        match self {
            Self::Vscode => "vscode-copilot",
            Self::Copilot => "copilot",
        }
    }

    /// The config file. VS Code's lives in its user profile. Copilot's in `$COPILOT_HOME`.
    fn file(self) -> Result<PathBuf, String> {
        Ok(match self {
            Self::Vscode if cfg!(target_os = "macos") => {
                user_home()?.join("Library/Application Support/Code/User/mcp.json")
            }
            Self::Vscode => match absolute_env("XDG_CONFIG_HOME") {
                Some(config) => config,
                None => user_home()?.join(".config"),
            }
            .join("Code/User/mcp.json"),
            Self::Copilot => copilot_home()?.join("mcp-config.json"),
        })
    }

    /// The object in the file that holds the servers.
    fn section(self) -> &'static str {
        match self {
            Self::Vscode => "servers",
            Self::Copilot => "mcpServers",
        }
    }

    /// The entry that runs `command`, passing `home` as `ZERODB_HOME` when set.
    fn entry(self, command: &str, home: Option<&str>) -> Value {
        let entry = match self {
            // No `--session`: the adapter names the session after the repo of the client's
            // first root, else of its working folder, as the session-start hook reads it.
            // VS Code reports roots. It doesn't resolve variables in the user `mcp.json`.
            Self::Vscode => json!({"type": "stdio", "command": command,
                "args": ["mcp", "--agent", self.agent()]}),
            Self::Copilot => json!({"type": "local", "command": command,
                "args": ["mcp", "--agent", self.agent()], "tools": ["*"]}),
        };
        with_home(entry, home)
    }
}

/// `object` with `"env": {"ZERODB_HOME": home}` added when `home` is set.
fn with_home(mut object: Value, home: Option<&str>) -> Value {
    if let Some(home) = home {
        object["env"] = json!({ HOME_ENV: home });
    }
    object
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

/// `$COPILOT_HOME` when set and absolute, else `~/.copilot`.
fn copilot_home() -> Result<PathBuf, String> {
    absolute_env("COPILOT_HOME").map_or_else(|| Ok(user_home()?.join(".copilot")), Ok)
}

/// The absolute path of the running binary, which must be UTF-8 to fit in JSON.
fn binary() -> Result<String, String> {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize);
    let exe = exe.map_err(|error| format!("cannot find the zerodb binary: {error}"))?;
    exe.into_os_string().into_string().map_err(|_| "the zerodb binary's path must be UTF-8".into())
}

/// Adds zerodb's entry to `harness`'s config and writes the hook, after creating the home.
/// An entry with connection flags is replaced only with `force`. Under `ZERODB_HOME`, the
/// entry and the hook pass it on. The other harness must pass the same home, as both share
/// the hook. Else nothing changes, unless `force`. The flags are checked first, so `force`
/// is never advised past a check that was not shown. Returns the exit code.
pub fn connect(harness: Harness, force: bool) -> u8 {
    finish(moved_home().and_then(|moved| {
        if !force {
            check_flags(harness, &crate::home::dir()?.join("data"))?;
        }
        let split = split_home(harness, moved.as_deref(), force)?;
        let home = crate::home::ensure()?;
        let exe = binary()?;
        let guard = (!force).then_some(home.data_dir.as_path());
        let entry = harness.entry(&exe, moved.as_deref());
        println!("{}", edit(harness, Some(entry), guard)?);
        println!("{}", set_hook(&exe, moved.as_deref())?);
        if let Some(split) = split {
            eprintln!("zerodb: {split}");
        }
        Ok(String::new())
    }))
}

/// Runs [`refuse_flags`] on `harness`'s current entry, before any file changes. `data` is
/// the home's data directory. A config that doesn't load is left for `edit` to report.
fn check_flags(harness: Harness, data: &Path) -> Result<(), String> {
    let path = harness.file()?;
    let Ok((_, root)) = load(&path) else { return Ok(()) };
    match root.get(harness.section()).and_then(|servers| servers.get("zerodb")) {
        Some(current) => refuse_flags(harness.name(), &path, current, data),
        None => Ok(()),
    }
}

/// Checks the home `harness` would pass against the other connected harness's. Both share
/// the hook, which reads 1 home, so a mismatch splits memory. It is refused before any file
/// changes. With `force`, it comes back as a note to print.
fn split_home(harness: Harness, home: Option<&str>, force: bool) -> Result<Option<String>, String> {
    let ours = home_words(home);
    for other in Harness::ALL.into_iter().filter(|other| *other != harness) {
        let name = other.name();
        // With HOME unset, the other config can't be located. It may still have been
        // connected from a shell where HOME was set, so its home is unknown.
        let file = match other.file() {
            Ok(file) => file,
            Err(why) if force => {
                return Ok(Some(format!(
                    "{name}'s config can't be located ({why}), so it may use another home than \
                     the one the shared sessionStart hook now reads, {ours}"
                )));
            }
            Err(why) => {
                return Err(format!(
                    "{name}'s config can't be located ({why}), so its zerodb entry may use \
                     another home than {ours}. Both share the sessionStart hook, which reads 1 \
                     home. Nothing changed. Set HOME, or add --force to go ahead anyway"
                ));
            }
        };
        let theirs = match entry_home(other, &file) {
            Ok(None) => continue,
            Ok(Some(theirs)) if theirs.as_deref().map(Path::new) == home.map(Path::new) => continue,
            Ok(Some(theirs)) => home_words(theirs.as_deref()),
            Err(why) => format!("an unknown home, as {why}"),
        };
        if force {
            return Ok(Some(format!(
                "{name} still uses {theirs}, but the shared sessionStart hook now reads {ours}. \
                 Connect {name} again under the same {HOME_ENV}"
            )));
        }
        return Err(format!(
            "{name}'s zerodb entry uses {theirs}, but this would use {ours}. Both share the \
             sessionStart hook, which reads 1 home. Nothing changed. Connect {name} again under \
             the same {HOME_ENV}, or disconnect it first. Or add --force to go ahead anyway"
        ));
    }
    Ok(None)
}

/// The home that `harness`'s zerodb entry in `path` passes in its `env`, `Some(None)` for
/// the default home. `None` when it has no entry. An empty `ZERODB_HOME` is the default home,
/// as [`crate::home::env_dir`] reads it. In a file that isn't plain JSON, an entry
/// counts as on the default home unless the file holds the text `ZERODB_HOME`. Then its home
/// is unknown, and the error says why.
fn entry_home(harness: Harness, path: &Path) -> Result<Option<Option<String>>, String> {
    let mut bytes = Vec::new();
    let read = match crate::config::open_regular(OpenOptions::new().read(true), path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        opened => opened.and_then(|mut file| file.read_to_end(&mut bytes)),
    };
    read.map_err(|error| format!("{} can't be read: {error}", path.display()))?;
    let holds = |text: &[u8]| bytes.windows(text.len()).any(|window| window == text);
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(root) => Ok(root
            .get(harness.section())
            .and_then(|servers| servers.get("zerodb"))
            .map(|entry| entry["env"][HOME_ENV].as_str().filter(|home| !home.is_empty()))
            .map(|home| home.map(str::to_owned))),
        Err(_) if !holds(b"\"zerodb\"") => Ok(None),
        Err(_) if !holds(HOME_ENV.as_bytes()) => Ok(Some(None)),
        Err(_) => Err(format!("{} is not plain JSON", path.display())),
    }
}

/// `home` in words: `ZERODB_HOME <path>`, or the default home for `None`.
fn home_words(home: Option<&str>) -> String {
    home.map_or_else(|| "the default home".into(), |home| format!("{HOME_ENV} {home}"))
}

/// `$ZERODB_HOME` for the entry and the hook, or `None` when it is unset. A harness started
/// from the Dock doesn't get the shell's variables, so its files must name the home.
/// Copilot expands variables in a hook's env, so a path with a `$` is refused.
fn moved_home() -> Result<Option<String>, String> {
    let Some(dir) = crate::home::env_dir()? else { return Ok(None) };
    let dir = dir.into_os_string().into_string();
    let dir = dir.map_err(|_| format!("{HOME_ENV} must be UTF-8 to go in a harness config"))?;
    if dir.contains('$') {
        return Err(format!(
            "{HOME_ENV} {dir} holds a $. Copilot expands variables in a hook's env, so the hook \
             would read another home. Nothing changed. Use a path without $"
        ));
    }
    Ok(Some(dir))
}

/// Removes zerodb's entry from `harness`'s config, and the hook once no harness has one.
/// Returns the exit code.
pub fn disconnect(harness: Harness) -> u8 {
    finish(edit(harness, None, None).and_then(|message| {
        println!("{message}");
        remove_hook()
    }))
}

fn finish(result: Result<String, String>) -> u8 {
    match result {
        Ok(message) => {
            if !message.is_empty() {
                println!("{message}");
            }
            0
        }
        Err(error) => {
            eprintln!("zerodb: {error}");
            1
        }
    }
}

/// Sets zerodb's entry to `wanted`, or removes it for `None`. With `guard`, the home's data
/// directory, an entry with connection flags is refused. Returns the message to print.
fn edit(harness: Harness, wanted: Option<Value>, guard: Option<&Path>) -> Result<String, String> {
    let (name, path) = (harness.name(), harness.file()?);
    let section = harness.section();
    let shown = path.display();
    let by_hand = match &wanted {
        Some(entry) => format!("add \"zerodb\": {entry} to \"{section}\" by hand"),
        None => format!("remove \"zerodb\" from \"{section}\" by hand"),
    };
    let unusable = |why: String| format!("{shown}: {why}. Nothing changed, {by_hand}");
    let (old, mut root) = load(&path).map_err(unusable)?;
    let servers = match root.entry(section).or_insert_with(|| json!({})) {
        Value::Object(servers) => servers,
        _ => return Err(unusable(format!("\"{section}\" is not an object"))),
    };
    let current = servers.get("zerodb").cloned();
    let verb = match (&wanted, &current) {
        (Some(entry), Some(current)) if entry == current => {
            return Ok(format!("zerodb is already connected to {name} ({shown})"));
        }
        (None, None) => return Ok(format!("zerodb is not connected to {name}")),
        (Some(entry), current) => {
            if let (Some(current), Some(data)) = (current, guard) {
                refuse_flags(name, &path, current, data)?;
            }
            servers.insert("zerodb".into(), entry.clone());
            if current.is_some() {
                "updated zerodb in"
            } else {
                "connected zerodb to"
            }
        }
        (None, Some(_)) => {
            servers.remove("zerodb");
            "disconnected zerodb from"
        }
    };
    let mut text = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("{shown}: cannot encode: {error}"))?;
    text.push('\n');
    let backup = write(&path, old.as_ref(), text.as_bytes())?;
    Ok(match backup {
        Some(backup) => {
            format!("{verb} {name}: {shown} (backup: {}, keys are now sorted)", backup.display())
        }
        None => format!("{verb} {name}: {shown}"),
    })
}

/// Flags that point an entry at a store other than the home.
const CONNECTION: [&str; 5] = ["--url", "--data-dir", "--config", "--token-file", "--tenant"];

/// Fails when `current`, the entry in `file`, passes connection flags, because its memory
/// would not follow a switch to the home, whose data directory is `data`.
fn refuse_flags(name: &str, file: &Path, current: &Value, data: &Path) -> Result<(), String> {
    let args = current["args"].as_array().map(Vec::as_slice).unwrap_or_default();
    let args: Vec<&str> = args.iter().filter_map(Value::as_str).collect();
    let mut used = Vec::new();
    for flag in CONNECTION {
        if let Some(at) = args.iter().position(|arg| *arg == flag) {
            // `--token-file`'s value is a path, not the token.
            used.push(args.get(at + 1).map_or(flag.to_owned(), |value| format!("{flag} {value}")));
        }
    }
    if used.is_empty() {
        return Ok(());
    }
    let (used, file, data) = (used.join(", "), file.display(), data.display());
    Err(format!(
        "{file}: the zerodb entry for {name} uses {used}. {file} was not changed.\n\
         Its memory stays in that store and would not follow a switch to the home.\n\
         To keep it, quit VS Code and Copilot CLI, then stop that store's server.\n\
         Move the files in its data directory into {data}.\n\
         Do that only while {data} is empty, and only if the old config's tenant is \"me\".\n\
         Or keep using the old entry.\n\
         Then, or to switch without it: zerodb connect {name} --force"
    ))
}

/// A file's bytes and mode.
type Saved = (Vec<u8>, u32);

/// The file's bytes and mode, and its JSON object. A missing file is an empty object.
fn load(path: &Path) -> Result<(Option<Saved>, Map<String, Value>), String> {
    let old = read(path)?;
    let root = match &old {
        Some((bytes, _)) => match serde_json::from_slice(bytes) {
            Ok(Value::Object(root)) => root,
            Ok(_) => return Err("not a JSON object".into()),
            Err(error) => return Err(format!("not plain JSON ({error})")),
        },
        None => Map::new(),
    };
    Ok((old, root))
}

/// The hook file, `hooks/zerodb.json` in `$COPILOT_HOME` or `~/.copilot`. VS Code's Copilot
/// chat and Copilot CLI both read it.
fn hook_file() -> Result<PathBuf, String> {
    Ok(copilot_home()?.join("hooks/zerodb.json"))
}

/// The hook file's text: a `sessionStart` hook that runs `exe hook session-start`, passing
/// `home` as `ZERODB_HOME` when set.
fn hook_text(exe: &str, home: Option<&str>) -> Result<String, String> {
    let bash = format!("'{}' hook session-start", exe.replace('\'', r"'\''"));
    let command = with_home(json!({"type": "command", "bash": bash, "timeoutSec": 5}), home);
    let hook = json!({"version": 1, "hooks": {"sessionStart": [command]}});
    let text = serde_json::to_string_pretty(&hook);
    Ok(text.map_err(|error| format!("cannot encode the hook: {error}"))? + "\n")
}

/// Writes the hook file for `exe` and `home`. Returns the message to print.
fn set_hook(exe: &str, home: Option<&str>) -> Result<String, String> {
    let (path, text) = (hook_file()?, hook_text(exe, home)?);
    let shown = path.display();
    let old = read(&path).map_err(|why| format!("{shown}: {why}. The hook was not written"))?;
    if old.as_ref().is_some_and(|(bytes, _)| *bytes == text.as_bytes()) {
        return Ok(format!("the sessionStart hook is already in place ({shown})"));
    }
    Ok(match write(&path, old.as_ref(), text.as_bytes())? {
        Some(backup) => {
            format!("updated the sessionStart hook: {shown} (backup: {})", backup.display())
        }
        None => format!("added the sessionStart hook: {shown}"),
    })
}

/// Whether `bytes` are exactly the hook zerodb writes for some absolute binary path, with
/// or without a `ZERODB_HOME`, so a hook from a moved or other zerodb binary counts too.
fn ours(bytes: &[u8]) -> bool {
    let hook: Value = serde_json::from_slice(bytes).unwrap_or_default();
    let command = &hook["hooks"]["sessionStart"][0];
    let bash = command["bash"].as_str().unwrap_or_default();
    let quoted = bash.strip_suffix(" hook session-start").and_then(|q| q.strip_prefix('\''));
    let exe = quoted.and_then(|q| q.strip_suffix('\'')).map(|q| q.replace(r"'\''", "'"));
    let exe = exe.filter(|exe| Path::new(exe).is_absolute());
    // Writing the hook again for that path and home must give the same bytes.
    let home = command["env"][HOME_ENV].as_str();
    exe.and_then(|exe| hook_text(&exe, home).ok()).is_some_and(|text| text.as_bytes() == bytes)
}

/// Whether `harness`'s config has a zerodb entry. A config that is not plain JSON, such as
/// one with comments, counts when it holds `"zerodb"`.
fn connected(harness: Harness) -> Result<bool, String> {
    let Some((bytes, _)) = read(&harness.file()?)? else { return Ok(false) };
    Ok(match serde_json::from_slice::<Value>(&bytes) {
        Ok(root) => root.get(harness.section()).is_some_and(|s| s.get("zerodb").is_some()),
        Err(_) => bytes.windows(8).any(|window| window == b"\"zerodb\""),
    })
}

/// Deletes the hook file once no harness has a zerodb entry, if zerodb wrote it. Returns
/// the message to print, empty when there is no hook file.
fn remove_hook() -> Result<String, String> {
    let path = hook_file()?;
    let shown = path.display();
    let bytes = match read(&path) {
        Ok(Some((bytes, _))) => bytes,
        Ok(None) => return Ok(String::new()),
        Err(why) => return Ok(format!("left {shown} alone: {why}")),
    };
    let kept = format!("kept the sessionStart hook ({shown})");
    for harness in Harness::ALL {
        let name = harness.name();
        match connected(harness) {
            Ok(true) => return Ok(format!("{kept}: zerodb is still connected to {name}")),
            Ok(false) => {}
            // A config that cannot be read may still have the entry.
            Err(error) => return Ok(format!("{kept}: cannot read {name}'s config: {error}")),
        }
    }
    if !ours(&bytes) {
        let why = "it differs from the hook zerodb writes. Delete it by hand to stop the block";
        return Ok(format!("left {shown} alone: {why}"));
    }
    std::fs::remove_file(&path).map_err(|error| format!("{shown}: cannot delete: {error}"))?;
    let parent = path.parent().unwrap_or(Path::new("/"));
    StdIo::new()
        .sync_dir(parent)
        .map_err(|error| format!("{}: cannot sync: {error}", parent.display()))?;
    Ok(format!("removed the sessionStart hook: {shown}"))
}

/// The file's bytes and mode, or `None` when it is missing. A symlink is refused, because
/// the rename would replace it with a regular file.
fn read(path: &Path) -> Result<Option<Saved>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(meta) if meta.is_symlink() => return Err("is a symlink".into()),
        Ok(_) => {}
    }
    let mut file = crate::config::open_regular(OpenOptions::new().read(true), path)
        .map_err(|error| format!("cannot read: {error}"))?;
    let mode = file.metadata().map_err(|error| format!("cannot read: {error}"))?.permissions();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .map_err(|error| format!("cannot read: {error}"))?;
    Ok(Some((bytes, mode.mode() & 0o777)))
}

/// Backs up `old`, then replaces `path` with `text` through a synced temp file and a rename.
/// Creates missing parent directories. Syncs every directory entry it makes. Returns the
/// backup's path.
fn write(path: &Path, old: Option<&Saved>, text: &[u8]) -> Result<Option<PathBuf>, String> {
    let shown = path.display();
    let parent = path.parent().ok_or_else(|| format!("{shown}: has no parent directory"))?;
    let existing = parent.ancestors().find(|dir| dir.exists()).unwrap_or(Path::new("/"));
    // New directories are private, as Copilot and XDG make them. Existing ones keep their mode.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|error| format!("{}: cannot create: {error}", parent.display()))?;
    let io = StdIo::new();
    let sync = |dir: &Path| {
        io.sync_dir(dir).map_err(|error| format!("{}: cannot sync: {error}", dir.display()))
    };
    let backup = match old {
        Some((bytes, mode)) => {
            let backup = save(path, bytes, *mode)?;
            sync(parent)?;
            Some(backup)
        }
        None => None,
    };
    let file_name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let hex = crate::random_hex(8).map_err(|error| format!("cannot read /dev/urandom: {error}"))?;
    let tmp = parent.join(format!(".{file_name}.zerodb-{hex}"));
    let written = create(&tmp, text, old.map(|(_, mode)| *mode))
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|error| format!("{shown}: cannot write: {error}"));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    // Deepest first, so each new directory's entry is durable once its parent is synced.
    for dir in parent.ancestors() {
        sync(dir)?;
        if dir == existing {
            break;
        }
    }
    Ok(backup)
}

/// Copies `bytes` to a new `<path>.zerodb-backup-<unix seconds>`, with a `-<n>` suffix when
/// that name is taken, and syncs it.
fn save(path: &Path, bytes: &[u8], mode: u32) -> Result<PathBuf, String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    // Built from the path's bytes, so a non-UTF-8 directory name stays intact.
    let mut base = path.as_os_str().to_owned();
    base.push(format!(".zerodb-backup-{seconds}"));
    for n in 0..100 {
        let mut backup = base.clone();
        if n > 0 {
            backup.push(format!("-{n}"));
        }
        let backup = PathBuf::from(backup);
        match create(&backup, bytes, Some(mode)) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                let _ = std::fs::remove_file(&backup);
                return Err(format!("{}: cannot write the backup: {error}", backup.display()));
            }
            Ok(()) => return Ok(backup),
        }
    }
    Err(format!("{}: too many backups in 1 second", Path::new(&base).display()))
}

/// Creates `path`, which must not exist, with `bytes` and `mode`, and syncs it. A new file
/// without `mode` is owner-only.
fn create(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    let mut file: File =
        OpenOptions::new().write(true).create_new(true).mode(mode.unwrap_or(0o600)).open(path)?;
    if let Some(mode) = mode {
        file.set_permissions(Permissions::from_mode(mode))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}
