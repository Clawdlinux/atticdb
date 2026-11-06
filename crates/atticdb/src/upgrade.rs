//! `atticdb upgrade [--yes] [--version <v>]`. It installs a release over this binary with that
//! release's own `install.sh`, which checks the download against `SHA256SUMS`. The old binary
//! stays as `atticdb.prev`. Then it stops the home's server and runs the new binary's doctor.
//! A binary that cargo or Homebrew installed gets that tool's command instead. Windows gets
//! the manual steps.

/// Runs `atticdb upgrade`. `yes` skips the question, and `version` pins the release. Returns
/// the exit code: 0 when it upgraded, was up to date or was told no. 1 when the check, the
/// download or the installer failed, or another tool owns the binary. 2 for a usage error, or
/// no terminal to ask on.
#[cfg(unix)]
pub fn upgrade(yes: bool, version: Option<&str>) -> u8 {
    match unix::run(yes, version) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("atticdb: {error}");
            error.code()
        }
    }
}

/// Prints the manual steps from the README, and returns 1. Windows has no `install.sh`, and
/// a running `atticdb.exe` can't be replaced.
#[cfg(not(unix))]
pub fn upgrade(yes: bool, version: Option<&str>) -> u8 {
    let _ = (yes, version);
    let exe = std::env::current_exe().ok();
    let dir = exe.as_deref().and_then(std::path::Path::parent);
    let dir = dir.map_or_else(
        || r"$env:LOCALAPPDATA\Programs\atticdb".to_owned(),
        |dir| dir.display().to_string(),
    );
    println!(
        "atticdb upgrade doesn't run on Windows. To upgrade by hand:
  1. Download atticdb-x86_64-pc-windows-msvc.zip and SHA256SUMS from
     https://github.com/Clawdlinux/atticdb/releases/latest
  2. Check the zip in PowerShell. The hash must match SHA256SUMS:
     (Get-FileHash .\\atticdb-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()
  3. Keep a copy of atticdb.exe, to roll back.
  4. Close your apps, and end atticdb.exe in Task Manager.
  5. Expand the zip over the old binary:
     Expand-Archive -Force .\\atticdb-x86_64-pc-windows-msvc.zip \"{dir}\"
  6. Run: atticdb doctor"
    );
    1
}

#[cfg(unix)]
mod unix {
    use crate::update;
    use std::fmt;
    use std::io::{BufRead, Write};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime};

    /// The releases page, and where `install.sh` comes from by default.
    const RELEASES: &str = "https://github.com/Clawdlinux/atticdb/releases";
    /// Moves the downloads to a mirror. `install.sh` reads it too.
    const BASE_ENV: &str = "ATTICDB_DOWNLOAD_BASE";
    /// The name `install.sh` writes in its folder.
    const NAME: &str = "atticdb";
    /// The old binary's name, beside the new one.
    const PREV: &str = "atticdb.prev";
    /// Where an earlier `atticdb.prev` waits while the installer runs.
    const PREV_OLD: &str = "atticdb.prev.old";
    /// Shown in every plan. Releases carry no store format version, so it can't be narrower.
    const WARNING: &str = "If this release changes the store format, the previous binary \
                           can't open the store after the first write.";
    /// Shown instead of [`WARNING`] when the release is older than this binary.
    const DOWNGRADE: &str = "This is an older release. It may not open a store a newer \
                             release has written. Roll back with atticdb.prev.";
    /// What it says when it can't ask.
    const NEEDS_TERMINAL: &str = "atticdb upgrade needs a terminal, or --yes.";
    /// The 1 line for a failed check.
    const NO_LATEST: &str =
        "cannot find the latest release. Check the network, or pass --version <v>.";
    /// The line for a binary under `.cargo/bin`.
    const CARGO: &str = "Installed by cargo. Run: cargo install --git \
                         https://github.com/Clawdlinux/atticdb atticdb --locked";
    /// The line for a binary that Homebrew installed.
    const BREW: &str = "Installed by Homebrew. Run: brew upgrade atticdb";
    /// How long the old server may take to free the lock after SIGTERM.
    const STOP_WAIT: Duration = Duration::from_secs(5);
    /// How long the server has to prove it is this data dir's, before it is signalled.
    const PROBE: Duration = Duration::from_secs(2);

    /// Why an upgrade stopped.
    #[derive(Debug)]
    pub(super) enum Error {
        /// A bad flag value, or no answer to the question. Exit 2.
        Usage(String),
        /// The check, the download or the install failed. Exit 1.
        Failed(String),
    }

    impl Error {
        /// The exit code.
        pub(super) fn code(&self) -> u8 {
            match self {
                Self::Usage(_) => 2,
                Self::Failed(_) => 1,
            }
        }
    }

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Usage(why) | Self::Failed(why) => f.write_str(why),
            }
        }
    }

    /// [`super::upgrade`]. Prints as it goes. Returns the exit code, or the error to print.
    pub(super) fn run(yes: bool, version: Option<&str>) -> Result<u8, Error> {
        let bad_version = || Error::Usage("--version must be a release such as 0.3.0".into());
        let pinned = version.map(|v| update::version_of(v).ok_or_else(bad_version)).transpose()?;
        let exe = this_binary()?;
        if let Some(line) = managed(&exe) {
            println!("{line}");
            return Ok(1);
        }
        if !yes && !crate::setup::terminal() {
            eprintln!("{NEEDS_TERMINAL}");
            return Ok(2);
        }
        let home = crate::home::dir().ok();
        let to = match pinned {
            Some(to) => to,
            None => {
                let latest = update::latest_fresh(home.as_deref(), SystemTime::now());
                let latest = latest.ok_or_else(|| Error::Failed(NO_LATEST.into()))?;
                if !wanted(&latest, crate::VERSION) {
                    println!("AtticDB {} is the latest.", crate::VERSION);
                    return Ok(0);
                }
                latest
            }
        };
        let prev = exe.with_file_name(PREV);
        println!("Upgrade AtticDB:");
        println!("  from: {}", crate::VERSION);
        println!("  to: {to}");
        println!("  notes: {RELEASES}/tag/v{to}");
        println!("  binary: {}, the old one kept as {}", exe.display(), prev.display());
        println!("{}", if older(&to, crate::VERSION) { DOWNGRADE } else { WARNING });
        if !yes {
            match ask("Upgrade? [y/N] ") {
                None => return Err(Error::Usage(format!("no answer on stdin. {NEEDS_TERMINAL}"))),
                Some(answer) if crate::setup::yes_no(&answer) == Some(true) => {}
                Some(_) => {
                    println!("Nothing changed.");
                    return Ok(0);
                }
            }
        }
        install(&exe, &prev, &to)?;
        println!("Installed AtticDB {to} at {}.", exe.display());
        if let Some(home) = home {
            stop_server(&home.join("data"));
        }
        // The new binary's doctor reports the new version. It also starts the new server.
        let _ = std::io::stdout().flush();
        let mut doctor = Command::new(&exe);
        doctor.arg("doctor").stdin(Stdio::null());
        crate::spawn(&mut doctor).and_then(|mut child| child.wait()).map_err(|error| {
            Error::Failed(format!("cannot run {} doctor: {error}", exe.display()))
        })?;
        Ok(0)
    }

    /// This binary's real path, which must be named `atticdb` as `install.sh` writes it.
    fn this_binary() -> Result<PathBuf, Error> {
        let exe = std::env::current_exe().and_then(std::fs::canonicalize);
        let exe =
            exe.map_err(|error| Error::Failed(format!("cannot find this binary: {error}")))?;
        if exe.file_name().is_none_or(|name| name != NAME) {
            let why = format!(
                "{} is not named atticdb, and install.sh writes atticdb. Install it again with \
                 install.sh.",
                exe.display()
            );
            return Err(Error::Failed(why));
        }
        Ok(exe)
    }

    /// The line pointing to the tool that installed `exe`, when it was cargo or Homebrew.
    fn managed(exe: &Path) -> Option<&'static str> {
        let path = exe.to_string_lossy();
        if path.contains("/.cargo/bin/") {
            Some(CARGO)
        } else if path.contains("/opt/homebrew/") || path.contains("/Cellar/") {
            Some(BREW)
        } else {
            None
        }
    }

    /// Whether `to`, a release's `x.y.z`, is an upgrade from `current`. A source build,
    /// `x.y.z+dev`, counts as older than release `x.y.z`. So it upgrades to that release or a
    /// newer one, but never to an older one.
    fn wanted(to: &str, current: &str) -> bool {
        match current.strip_suffix("+dev") {
            Some(base) => {
                update::version_of(to).is_some() && (to == base || update::newer(to, base))
            }
            None => update::newer(to, current),
        }
    }

    /// Whether `to`, a release's `x.y.z`, is older than `current`: `newer(current, to)`. A
    /// source build, `x.y.z+dev`, compares as `x.y.z`, as in [`wanted`].
    fn older(to: &str, current: &str) -> bool {
        update::newer(current.strip_suffix("+dev").unwrap_or(current), to)
    }

    /// Prints `prompt` and reads 1 answer from stdin, trimmed. `None` at the end of stdin.
    fn ask(prompt: &str) -> Option<String> {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => {
                println!();
                None
            }
            Ok(_) => Some(answer.trim().to_owned()),
        }
    }

    /// Downloads the `install.sh` of release `to` to a temp file. Then it keeps `exe` as
    /// `prev`, and runs the script for `exe`'s folder. The temp file is removed either way.
    fn install(exe: &Path, prev: &Path, to: &str) -> Result<(), Error> {
        let base = match std::env::var_os(BASE_ENV).filter(|base| !base.is_empty()) {
            Some(base) => Some(base.into_string().map_err(|_| {
                Error::Failed(format!("{BASE_ENV} is not UTF-8. Nothing changed."))
            })?),
            None => None,
        };
        let root = base.as_deref().unwrap_or(RELEASES).trim_end_matches('/');
        let url = format!("{root}/download/v{to}/install.sh");
        let hex = crate::random_hex(8).map_err(|error| Error::Failed(error.to_string()))?;
        let script = std::env::temp_dir().join(format!("atticdb-upgrade.{hex}.sh"));
        update::download(&url, &script).map_err(|why| {
            Error::Failed(format!("cannot download {url}: {why}. Nothing changed."))
        })?;
        let installed = run_installer(exe, prev, to, base.as_deref(), &script);
        let _ = std::fs::remove_file(&script);
        installed
    }

    /// Copies `exe` to `prev`, then runs `sh <script>` pinned to `to` and to `exe`'s folder.
    /// An earlier `prev` waits as `prev.old` meanwhile. It is removed on success.
    /// When the script fails, `exe` is as it was. The copy goes back if the script replaced
    /// it. Otherwise the copy is removed. Either way the earlier `prev` comes back.
    fn run_installer(
        exe: &Path,
        prev: &Path,
        to: &str,
        base: Option<&str>,
        script: &Path,
    ) -> Result<(), Error> {
        let dir = exe.parent().unwrap_or(Path::new("/"));
        let before = std::fs::metadata(exe)
            .map_err(|error| Error::Failed(format!("{}: {error}", exe.display())))?;
        let old = exe.with_file_name(PREV_OLD);
        let aside = set_aside(prev, &old).map_err(|error| {
            let prev = prev.display();
            Error::Failed(format!("cannot move {prev} aside: {error}. Nothing changed."))
        })?;
        if let Err(error) = keep(exe, prev) {
            bring_back(aside, &old, prev);
            let prev = prev.display();
            let why = format!("cannot copy the binary to {prev}: {error}. Nothing changed.");
            return Err(Error::Failed(why));
        }
        let mut command = Command::new("/bin/sh");
        command.arg(script).env("ATTICDB_VERSION", to).env("ATTICDB_INSTALL_DIR", dir);
        if let Some(base) = base {
            command.env(BASE_ENV, base);
        }
        // The installer needs no AtticDB token.
        command.env_remove(crate::client::TOKEN_ENV).stdin(Stdio::null());
        let _ = std::io::stdout().flush();
        let why = match crate::spawn(&mut command).and_then(|mut child| child.wait()) {
            Ok(status) if status.success() => {
                if aside {
                    let _ = std::fs::remove_file(&old);
                }
                return Ok(());
            }
            Ok(status) => status.to_string(),
            Err(error) => format!("cannot run /bin/sh: {error}"),
        };
        let replaced = std::fs::symlink_metadata(exe)
            .map_or(true, |now| (now.dev(), now.ino()) != (before.dev(), before.ino()));
        if !replaced {
            let _ = std::fs::remove_file(prev);
        } else if let Err(error) = std::fs::rename(prev, exe) {
            // `prev` holds the only copy of the old binary now, so `prev.old` stays put.
            let (exe, prev) = (exe.display(), prev.display());
            return Err(Error::Failed(format!(
                "the installer failed ({why}), and {exe} could not be put back: {error}. The old \
                 binary is {prev}."
            )));
        }
        bring_back(aside, &old, prev);
        Err(Error::Failed(format!("the installer failed ({why}). {} is unchanged.", exe.display())))
    }

    /// Renames an existing `prev` to `old`, replacing any `old` an interrupted run left.
    /// Returns whether there was one.
    fn set_aside(prev: &Path, old: &Path) -> std::io::Result<bool> {
        match std::fs::symlink_metadata(prev) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
            Ok(_) => std::fs::rename(prev, old).map(|()| true),
        }
    }

    /// Renames `old` back to `prev` when [`set_aside`] moved it. A failure is only reported,
    /// as the binary itself is fine.
    fn bring_back(aside: bool, old: &Path, prev: &Path) {
        if !aside {
            return;
        }
        if let Err(error) = std::fs::rename(old, prev) {
            let (old, prev) = (old.display(), prev.display());
            eprintln!("atticdb: cannot rename {old} back to {prev}: {error}.");
        }
    }

    /// Copies `exe` to `prev` through a temp file beside it, renamed over `prev`. So a link at
    /// `prev` is replaced, never followed. The copy keeps the mode.
    fn keep(exe: &Path, prev: &Path) -> std::io::Result<()> {
        let tmp = prev.with_file_name(format!(".{PREV}.{}", crate::random_hex(8)?));
        let copied = std::fs::copy(exe, &tmp).and_then(|_| std::fs::rename(&tmp, prev));
        if copied.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        copied
    }

    /// Stops the server holding the lock on `data`, the home's data dir, with SIGTERM. Then it
    /// waits up to [`STOP_WAIT`] for the lock to free. It signals only a server that proved it
    /// is this dir's. That server answered on the socket in `LOCK` with the `LOCK` instance
    /// id. A pid from a `LOCK` nothing vouches for is never signalled.
    fn stop_server(data: &Path) {
        let Some(holder) = crate::serve::lock_holder(data) else { return };
        let target = match verified_pid(data, &holder) {
            Ok(target) => target,
            Err(why) => {
                eprintln!("atticdb: the server was not stopped: {why}. Stop it by hand.");
                return;
            }
        };
        let pid = target.as_raw_pid();
        let sent = rustix::process::kill_process(target, rustix::process::Signal::TERM);
        if let Err(error) = sent {
            eprintln!("atticdb: cannot stop the server, pid {pid}: {error}.");
            return;
        }
        let deadline = Instant::now() + STOP_WAIT;
        while crate::serve::lock_holder(data).is_some() {
            if Instant::now() >= deadline {
                eprintln!("atticdb: the server, pid {pid}, still runs after 5 s. Stop it by hand.");
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("Stopped the server, pid {pid}.");
    }

    /// The pid in `holder`, the text of the held `LOCK` in `data`. Discovery must trust that
    /// server, and read the same pid. Never this process, nor pid 0 or 1.
    fn verified_pid(data: &Path, holder: &str) -> Result<rustix::process::Pid, String> {
        let pid = holder.strip_prefix("pid=").and_then(|rest| rest.split(' ').next());
        let pid: u32 =
            pid.and_then(|pid| pid.trim_end().parse().ok()).ok_or("LOCK names no pid")?;
        let target = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw);
        let target = target.filter(|_| pid > 1 && pid != std::process::id());
        let target = target.ok_or_else(|| format!("LOCK names pid {pid}"))?;
        let found = crate::launch::discover(data, Instant::now() + PROBE);
        let (stream, found) = found.map_err(|refusal| refusal.to_string())?;
        drop(stream);
        if found.pid != Some(pid) {
            return Err("LOCK changed while it was checked".into());
        }
        Ok(target)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wanted_only_for_newer() {
            assert!(wanted("0.3.0", "0.2.0"));
            assert!(!wanted("0.2.0", "0.2.0"));
            assert!(!wanted("0.1.0", "0.2.0"));
            // A source build is older than its own release, and newer than older ones.
            assert!(wanted("0.2.0", "0.2.0+dev"));
            assert!(wanted("0.10.0", "0.2.0+dev"));
            assert!(!wanted("0.1.9", "0.2.0+dev"));
            assert!(!wanted("nightly", "0.2.0+dev"));
            assert!(!wanted("0.2.0+dev", "0.2.0+dev"));
        }

        #[test]
        fn older_for_a_downgrade() {
            assert!(older("0.1.0", "0.2.0"));
            assert!(older("0.1.9", "0.2.0+dev"));
            assert!(!older("0.2.0", "0.2.0"));
            assert!(!older("0.2.0", "0.2.0+dev"));
            assert!(!older("0.3.0", "0.2.0"));
            assert!(!older("0.3.0", "0.2.0+dev"));
        }

        #[test]
        fn cargo_and_brew_by_path() {
            let cases = [
                ("/home/u/.cargo/bin/atticdb", Some(CARGO)),
                ("/opt/homebrew/bin/atticdb", Some(BREW)),
                ("/opt/homebrew/Cellar/atticdb/0.1.0/bin/atticdb", Some(BREW)),
                ("/usr/local/Cellar/atticdb/0.1.0/bin/atticdb", Some(BREW)),
                ("/home/linuxbrew/.linuxbrew/Cellar/atticdb/0.1.0/bin/atticdb", Some(BREW)),
                ("/home/u/.local/bin/atticdb", None),
                ("/usr/local/bin/atticdb", None),
                ("/home/u/cargo/bin/atticdb", None),
            ];
            for (path, line) in cases {
                assert_eq!(managed(Path::new(path)), line, "{path}");
            }
        }

        #[test]
        fn only_a_verified_pid() {
            let dir = std::env::temp_dir();
            let me = std::process::id();
            for holder in ["", "instance=1", "pid=x", "pid=0 ", "pid=1 ", &format!("pid={me} ")] {
                assert!(verified_pid(&dir, holder).is_err(), "{holder}");
            }
            // A pid with no server behind it.
            let err =
                verified_pid(&dir.join("no-such-data"), "pid=4242 instance=1 unix=/x tcp=none");
            assert!(matches!(&err, Err(why) if why.starts_with("LOCK: ")), "{err:?}");
        }
    }
}
