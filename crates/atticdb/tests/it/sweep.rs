use crate::support::{self, Dir, Process};
use std::fs::File;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::Duration;

/// Set in the child that [`sweep_runs_on_the_test_dir`] starts.
const CHILD: &str = "ATTICDB_SWEEP_CHILD";

/// The hex pid of a process that has exited and been reaped.
fn dead_pid() -> String {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    format!("{pid:x}")
}

/// The sweep stops a server that a killed test run left behind, and removes its folder (#227).
/// It leaves alone this process's folder, a running pid's folder and other names. It never
/// signals a pid in a `LOCK` that doesn't serve that folder, as when the pid was reused.
#[test]
fn sweep_stops_leftovers() {
    let dir = Dir::new();
    let root = &dir.0;
    support::init_config(&dir);
    let dead = dead_pid();
    let mut sleep = Command::new("sleep").arg("30").spawn().unwrap();
    let running = format!("{:x}", sleep.id());

    // A server left behind by the dead test process.
    let data = root.join(format!("{dead}-0/x/data"));
    std::fs::create_dir_all(data.parent().unwrap()).unwrap();
    let args = ["serve", "--config", "c.json", "--data-dir", data.to_str().unwrap()];
    let mut server = Process::spawn(root, &[&args[..], &["--listen", "127.0.0.1:0"]].concat());
    server.ready();
    assert!(support::locked(&data));

    // A held `LOCK` that names a running process which doesn't serve that folder.
    let reused = root.join(format!("{dead}-1/x/data"));
    std::fs::create_dir_all(&reused).unwrap();
    std::fs::write(reused.join("LOCK"), format!("pid={} instance=0 tcp=none\n", sleep.id()))
        .unwrap();
    let held = File::open(reused.join("LOCK")).unwrap();
    held.lock().unwrap();

    let kept = [format!("{:x}-0", std::process::id()), format!("{running}-0"), "notes".into()];
    for name in &kept {
        let data = root.join(name).join("x/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("LOCK"), "pid=1 instance=0 tcp=none\n").unwrap();
    }

    support::sweep(root);

    let (status, output) = server.wait_up_to(Duration::from_secs(10));
    assert_eq!(status.signal(), Some(9), "the sweep stops the server: {output}");
    assert!(!root.join(format!("{dead}-0")).exists(), "its folder is removed");
    assert!(!root.join(format!("{dead}-1")).exists(), "the reused pid's folder is removed");
    assert!(sleep.try_wait().unwrap().is_none(), "the reused pid is not signaled");
    for name in &kept {
        assert!(root.join(name).join("x/data/LOCK").exists(), "{name} is left alone");
    }
    assert!(root.join("c.json").exists(), "a file in the root is left alone");
    drop(held);
    sleep.kill().unwrap();
    sleep.wait().unwrap();
}

/// With `ATTICDB_TEST_DIR` set, the first [`Dir`] in a test process sweeps that root.
#[test]
fn sweep_runs_on_the_test_dir() {
    if std::env::var_os(CHILD).is_some() {
        drop(Dir::new());
        return;
    }
    let dir = Dir::new();
    let root = dir.0.join("r");
    let stale = root.join(format!("{}-0", dead_pid()));
    std::fs::create_dir_all(stale.join("x/data")).unwrap();
    std::fs::write(stale.join("x/data/LOCK"), "pid=1 instance=0 tcp=none\n").unwrap();
    let mut test = Command::new(std::env::current_exe().unwrap());
    test.args(["--exact", "sweep::sweep_runs_on_the_test_dir", "--test-threads=1"]);
    let out = test.env(CHILD, "1").env("ATTICDB_TEST_DIR", &root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stdout.contains(" 1 passed"), "{stdout}{stderr}");
    assert!(!stale.exists(), "the child swept {}", root.display());
    assert!(root.exists(), "the root itself stays");
}
