//! Small helpers for the Windows port (#137). Unix code keeps its own calls.

/// The random source, as error messages name it.
#[cfg(unix)]
pub(crate) const RANDOM_SOURCE: &str = "/dev/urandom";
/// The random source, as error messages name it.
#[cfg(not(unix))]
pub(crate) const RANDOM_SOURCE: &str = "the system random source";

/// The error for a failed read of [`RANDOM_SOURCE`].
pub(crate) fn no_random(error: std::io::Error) -> String {
    format!("cannot read {RANDOM_SOURCE}: {error}")
}

/// A `\\?\` path as other programs can read it: `\\?\UNC\server\share` becomes
/// `\\server\share`, and `\\?\C:\x` becomes `C:\x`. Any other path stays as is.
#[cfg(any(windows, test))]
pub(crate) fn plain_path(path: String) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_owned(),
        None => path,
    }
}

/// `std::fs::canonicalize`. On Windows without the `\\?\` prefix, by [`plain_path`], so
/// it compares equal to [`crate::connect::binary`].
pub(crate) fn real_path(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let real = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    let real = match real.into_os_string().into_string() {
        Ok(text) => plain_path(text).into(),
        Err(text) => text.into(),
    };
    Ok(real)
}

/// Whether `path` is `root` or inside it, by whole components. Case is ignored, as Windows
/// ignores it, so `C:\Users\Me` holds `c:\users\me\x`. A string prefix would also take
/// `C:\Users\Mel`.
#[cfg(any(windows, test))]
pub(crate) fn within(path: &std::path::Path, root: &std::path::Path) -> bool {
    let fold = |part: std::path::Component| part.as_os_str().to_string_lossy().to_lowercase();
    let mut parts = path.components();
    root.components().all(|part| parts.next().is_some_and(|next| fold(next) == fold(part)))
}

/// Refuses `path` unless it is inside `%LOCALAPPDATA%` or `%USERPROFILE%`. `what` names it in
/// the error. Windows gets no owner or mode check, so the profile's access list is the guard.
/// Both sides are resolved first: made absolute, with links and short 8.3 names followed.
#[cfg(windows)]
pub(crate) fn check_in_profile(path: &std::path::Path, what: &str) -> Result<(), String> {
    let roots = ["LOCALAPPDATA", "USERPROFILE"].into_iter().filter_map(std::env::var_os);
    let roots: Vec<_> = roots
        .map(std::path::PathBuf::from)
        .filter(|root| root.is_absolute())
        .filter_map(|root| resolve(&root).ok())
        .collect();
    let resolved = resolve(path).map_err(|error| format!("{what} {}: {error}", path.display()))?;
    if roots.iter().any(|root| within(&resolved, root)) {
        return Ok(());
    }
    Err(format!(
        "{what} {} must be inside %LOCALAPPDATA% or %USERPROFILE% (D92). On Windows AtticDB \
         checks no access lists, so it trusts only the user profile.",
        path.display()
    ))
}

/// `path` made absolute, with its longest existing part resolved by [`real_path`].
#[cfg(windows)]
fn resolve(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut rest = Vec::new();
    let mut base = absolute.as_path();
    loop {
        if let Ok(real) = real_path(base) {
            return Ok(rest.into_iter().rev().fold(real, |path, name| path.join(name)));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                base = parent;
            }
            _ => return Ok(absolute.clone()),
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(not(unix))]
pub(crate) use modes::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

/// Mode setters that do nothing, so code shared with Unix reads the same on Windows. Windows
/// has no mode bits. A folder in the user's profile is private by its inherited access list.
#[cfg(not(unix))]
mod modes {
    /// [`std::os::unix::fs::DirBuilderExt`] without effect.
    pub(crate) trait DirBuilderExt {
        /// Ignores `mode`.
        fn mode(&mut self, mode: u32) -> &mut Self;
    }

    impl DirBuilderExt for std::fs::DirBuilder {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }

    /// [`std::os::unix::fs::OpenOptionsExt`] without effect.
    pub(crate) trait OpenOptionsExt {
        /// Ignores `mode`.
        fn mode(&mut self, mode: u32) -> &mut Self;
    }

    impl OpenOptionsExt for std::fs::OpenOptions {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }

    /// The mode getter of [`std::os::unix::fs::PermissionsExt`], from the read-only flag.
    pub(crate) trait PermissionsExt {
        /// 0o400 for a read-only file, else 0o600.
        fn mode(&self) -> u32;
    }

    impl PermissionsExt for std::fs::Permissions {
        fn mode(&self) -> u32 {
            if self.readonly() {
                0o400
            } else {
                0o600
            }
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    // Process creation flags, from
    // https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// How long `taskkill` may take.
    const KILL_TIMEOUT: Duration = Duration::from_secs(10);

    /// A server that outlives this process. It gets no console and its own process group,
    /// so Ctrl-C here does not stop it. It must hold no handle of this process either, its
    /// stdio or one its parent passed down, such as PowerShell's stdout under a Copilot hook.
    /// Otherwise a caller up the chain would wait for end of file until the server exits.
    pub(crate) fn detach(command: &mut Command) {
        atticdb_sys::disinherit_all();
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    /// A short-lived helper: no console window pops up. It gets only the stdio its command
    /// sets, never a copy of this process's own. Otherwise a server it starts, such as the
    /// hook's, could hold this process's stdout open, and this process's caller would wait.
    pub(crate) fn no_window(command: &mut Command) {
        atticdb_sys::disinherit_stdio();
        command.creation_flags(CREATE_NO_WINDOW);
    }

    /// The folder a detached server runs in: the system drive root, so it pins no folder.
    pub(crate) fn root_dir() -> PathBuf {
        let drive = std::env::var_os("SystemDrive").filter(|drive| !drive.is_empty());
        let mut root = drive.unwrap_or_else(|| "C:".into());
        root.push("\\");
        PathBuf::from(root)
    }

    /// A program in the Windows folder, `%SystemRoot%`, by its path there. A bare name
    /// would be searched for in the current folder and on `PATH` first.
    pub(crate) fn system_program(path: &str) -> PathBuf {
        let root = std::env::var_os("SystemRoot").filter(|root| !root.is_empty());
        PathBuf::from(root.unwrap_or_else(|| r"C:\Windows".into())).join(path)
    }

    /// Kills `child` and every process it started with `taskkill /T /F`. The caller then
    /// kills and reaps `child`. Until then `child` holds its process handle, so Windows
    /// cannot reuse its pid.
    pub(crate) fn kill_tree(child: &Child) {
        let mut command = Command::new(system_program(r"System32\taskkill.exe"));
        command.args(["/PID", &child.id().to_string(), "/T", "/F"]);
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut command);
        if let Ok(mut taskkill) = command.spawn() {
            let deadline = Instant::now() + KILL_TIMEOUT;
            while matches!(taskkill.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = taskkill.kill();
            let _ = taskkill.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn within_compares_whole_components() {
        use std::path::Path;
        let root = Path::new("/home/me");
        for (path, inside) in
            [("/home/me", true), ("/home/me/x", true), ("/home/mel", false), ("/home", false)]
        {
            assert_eq!(super::within(Path::new(path), root), inside, "{path}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn within_compares_whole_components_without_case() {
        use std::path::Path;
        let root = Path::new(r"C:\Users\Me");
        for (path, inside) in [
            (r"C:\Users\Me", true),
            (r"c:\users\me\AppData\atticdb", true),
            (r"C:\Users\Mel", false),
            (r"C:\Users", false),
            (r"D:\Users\Me", false),
            (r"\\server\share\Users\Me", false),
        ] {
            assert_eq!(super::within(Path::new(path), root), inside, "{path}");
        }
    }

    #[test]
    fn plain_path_drops_the_verbatim_prefix() {
        for (path, plain) in [
            (r"\\?\C:\Users\me\atticdb.exe", r"C:\Users\me\atticdb.exe"),
            (r"\\?\UNC\server\share\atticdb.exe", r"\\server\share\atticdb.exe"),
            (r"C:\atticdb.exe", r"C:\atticdb.exe"),
            ("/usr/local/bin/atticdb", "/usr/local/bin/atticdb"),
        ] {
            assert_eq!(super::plain_path(path.into()), plain);
        }
    }
}
