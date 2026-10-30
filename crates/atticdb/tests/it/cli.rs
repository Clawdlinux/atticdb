use crate::support::{self, atticdb, data_dir, Dir, Holders};
use std::process::Stdio;

/// `--version` and `-V` print the name and the version on stdout and exit 0. They take no
/// other args. The home is never read, so a relative `ATTICDB_HOME` does not matter. A source
/// build prints the crate version plus `+dev`. A release build prints `ATTICDB_RELEASE_VERSION`.
#[test]
fn version_flag() {
    let dir = Dir::new();
    let want = match option_env!("ATTICDB_RELEASE_VERSION").filter(|v| !v.is_empty()) {
        Some(release) => format!("atticdb {release}\n"),
        None => format!("atticdb {}+dev\n", env!("CARGO_PKG_VERSION")),
    };
    assert_eq!(format!("atticdb {}\n", atticdb::VERSION), want);
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

/// A first `append` on a fresh home starts a server and succeeds. Waiting for it is the
/// normal path, so stderr has no "no trusted server" line.
#[test]
fn first_run_is_quiet() {
    let dir = Dir::new();
    let home = dir.0.join("h");
    std::fs::create_dir_all(&home).unwrap();
    let _holders = Holders(vec![data_dir(&home)]);
    let (code, out, err) = atticdb(&home, &home, &["append", "--content", "hi"], &[], b"", 0);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("\"committed\":true"), "{out}");
    assert!(err.contains("atticdb: started server pid "), "{err}");
    assert!(!err.contains("no trusted server"), "{err}");
}
