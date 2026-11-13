use crate::support::{self, Dir, Process};
use std::fs::File;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Set in the child that [`sweep_runs_on_the_test_dir`] starts.
const CHILD: &str = "ATTICDB_SWEEP_CHILD";

/// The hex pid of a process that has exited and been reaped.
fn dead_pid() -> String {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    format!("{pid:x}")
}

/// Makes `root/name` with `mode`, as [`Dir::new`] would without its marker, and
/// `root/name/x/data`. Returns the data folder.
fn folder(root: &Path, name: &str, mode: u32) -> PathBuf {
    let top = root.join(name);
    std::fs::DirBuilder::new().mode(0o700).create(&top).unwrap();
    support::chmod(&top, mode);
    let data = top.join("x/data");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&data).unwrap();
    data
}

/// A `sleep 600` that is killed on drop, also when a test fails. Its output goes nowhere, so
/// it never holds the test's pipes open.
struct Sleep(Child);

impl Sleep {
    fn start() -> Self {
        let mut command = Command::new("sleep");
        command.arg("600").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        Self(command.spawn().unwrap())
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A real `atticdb serve` on `data`, with the config `c.json` in `cwd`, once it serves.
fn serve(cwd: &Path, data: &Path) -> Process {
    let args = ["serve", "--config", "c.json", "--data-dir", data.to_str().unwrap()];
    let mut server = Process::spawn(cwd, &[&args[..], &["--listen", "127.0.0.1:0"]].concat());
    server.ready();
    assert!(support::locked(data), "{} is locked", data.display());
    server
}

/// The sweep stops a server that a killed test run left behind, and removes its folder (#227).
/// It never signals a pid in a `LOCK` that doesn't serve that folder, as when the pid was
/// reused. It never follows a symlink out of a leftover. It leaves alone this process's
/// folder, a running pid's folder and its server, and any other name or mode.
#[test]
fn sweep_stops_leftovers() {
    let dir = Dir::new();
    let root = &dir.0;
    support::init_config(&dir);
    let dead = dead_pid();
    let mut sleep = Sleep::start();
    let running = format!("{:x}", sleep.0.id());

    // A server left behind by the dead test process.
    let left = folder(root, &format!("{dead}-0"), 0o700);
    let mut stopped = serve(root, &left);

    // A symlink out of a leftover, to a folder with a live server and a plain file.
    let outside = Dir::new();
    let target = outside.0.join("x/data");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&target).unwrap();
    std::fs::write(outside.0.join("file"), "kept").unwrap();
    let mut beyond = serve(root, &target);
    let linked = root.join(format!("{dead}-2"));
    std::fs::DirBuilder::new().mode(0o700).create(&linked).unwrap();
    std::os::unix::fs::symlink(&outside.0, linked.join("link")).unwrap();

    // A live server in the folder of a running pid.
    let live = folder(root, &format!("{running}-0"), 0o700);
    let mut alive = serve(root, &live);

    let kept = [
        (format!("{:x}-0", std::process::id()), 0o700),
        ("notes".to_owned(), 0o700),
        (format!("0{dead}-0"), 0o700),
        (format!("{dead}-01"), 0o700),
        (format!("{dead}-3"), 0o755),
    ];
    for (name, mode) in &kept {
        let data = folder(root, name, *mode);
        std::fs::write(data.join("LOCK"), "pid=1 instance=0 tcp=none\n").unwrap();
    }

    support::sweep(root, false);

    let (status, output) = stopped.wait_up_to(Duration::from_secs(10));
    assert_eq!(status.signal(), Some(9), "the sweep stops the server: {output}");
    assert!(!root.join(format!("{dead}-0")).exists(), "its folder is removed");
    assert!(!linked.exists(), "the folder with the link is removed");
    assert!(beyond.child.try_wait().unwrap().is_none(), "{}", beyond.stderr());
    assert!(support::locked(&target), "the server behind the link keeps its lock");
    assert_eq!(std::fs::read_to_string(outside.0.join("file")).unwrap(), "kept");
    assert!(alive.child.try_wait().unwrap().is_none(), "{}", alive.stderr());
    assert!(support::locked(&live), "the running pid's server keeps its lock");
    for (name, _) in &kept {
        assert!(root.join(name).join("x/data/LOCK").exists(), "{name} is left alone");
    }
    assert!(root.join("c.json").exists(), "a file in the root is left alone");

    // A held `LOCK` that names a running process which doesn't serve that folder, swept on its
    // own. The lock is let go about 1 s into the sweep, so it needn't wait out 15 s.
    let solo = Dir::new();
    let reused = folder(&solo.0, &format!("{dead}-1"), 0o700);
    std::fs::write(reused.join("LOCK"), format!("pid={} instance=0 tcp=none\n", sleep.0.id()))
        .unwrap();
    let held = File::open(reused.join("LOCK")).unwrap();
    held.lock().unwrap();
    assert!(support::locked(&reused), "the reused pid's LOCK is held");
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        drop(held);
    });
    let started = Instant::now();
    support::sweep(&solo.0, false);
    release.join().unwrap();
    // A free lock ends the sweep after 500 ms, so this shows it saw the lock held.
    assert!(started.elapsed() >= Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(sleep.0.try_wait().unwrap().is_none(), "the reused pid is not signaled");
    assert!(!solo.0.join(format!("{dead}-1")).exists(), "the reused pid's folder is removed");
}

/// With `ATTICDB_TEST_DIR` set, the first [`Dir`] in a test process sweeps that root. There
/// a leftover also needs the marker that [`Dir::new`] writes.
#[test]
fn sweep_runs_on_the_test_dir() {
    if std::env::var_os(CHILD).is_some() {
        drop(Dir::new());
        return;
    }
    let dir = Dir::new();
    let root = dir.0.join("r");
    std::fs::create_dir(&root).unwrap();
    let dead = dead_pid();
    let open = folder(&root, &format!("{dead}-0"), 0o755);
    let marked = folder(&root, &format!("{dead}-1"), 0o700);
    let unmarked = folder(&root, &format!("{dead}-2"), 0o700);
    for data in [&open, &marked, &unmarked] {
        std::fs::write(data.join("LOCK"), "pid=1 instance=0 tcp=none\n").unwrap();
    }
    std::fs::write(root.join(format!("{dead}-1")).join(support::MARKER), "").unwrap();
    let mut test = Command::new(std::env::current_exe().unwrap());
    test.args(["--exact", "sweep::sweep_runs_on_the_test_dir", "--test-threads=1"]);
    let out = test.env(CHILD, "1").env("ATTICDB_TEST_DIR", &root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stdout.contains(" 1 passed"), "{stdout}{stderr}");
    assert!(!marked.exists(), "the child swept {}", root.display());
    assert!(open.exists(), "a mode 0755 folder with no marker stays");
    assert!(unmarked.exists(), "a folder with no marker stays");
    let names: Vec<_> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 2, "the child removed its own folder: {names:?}");
}
