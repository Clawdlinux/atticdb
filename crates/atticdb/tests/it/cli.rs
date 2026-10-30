use crate::support::{self, Dir};
use std::process::Stdio;

/// `--version` and `-V` print the name and the crate version on stdout and exit 0. They take
/// no other args. The home is never read, so a relative `ATTICDB_HOME` does not matter.
#[test]
fn version_flag() {
    let dir = Dir::new();
    let want = format!("atticdb {}\n", env!("CARGO_PKG_VERSION"));
    for flag in ["--version", "-V"] {
        let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
        command.arg(flag).current_dir(&dir.0).env("HOME", dir.0.join("h"));
        let out = command.env("ATTICDB_HOME", "rel").stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{flag}");
        assert!(out.stderr.is_empty(), "{flag}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!dir.0.join("h").exists() && !dir.0.join("rel").exists(), "{flag}: no home");

        let mut command = support::command(env!("CARGO_BIN_EXE_atticdb"));
        command.args([flag, "extra"]).current_dir(&dir.0).env("HOME", dir.0.join("h"));
        let out = command.stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag} extra");
        assert!(String::from_utf8_lossy(&out.stderr).contains("atticdb --version | -V"));
    }
}
