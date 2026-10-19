#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use zerodb_tape::sim::SimClock;
use zerodb_tape::Error;
use zerodb_tape::{Clock, Io, SimDisk, StdIo, SystemClock};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::current_dir().unwrap().join("target").join(format!(
            "io-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn filesystem_operations_through_trait_object() {
    let dir = TestDir::new();
    let io: Arc<dyn Io> = Arc::new(StdIo::new());
    let path = dir.0.join("data");
    assert!(!io.exists(&path).unwrap());
    let file = io.create(&path).unwrap();
    assert!(io.available_bytes(&dir.0).unwrap() <= io.total_bytes(&dir.0).unwrap());
    io.allocate(file, 32).unwrap();
    io.write_at(file, 4, b"hello").unwrap();
    io.sync_data(file).unwrap();
    io.sync_dir(&dir.0).unwrap();
    assert_eq!(io.len(file).unwrap(), 32);
    let reopened = io.open(&path).unwrap();
    let mut bytes = [0; 8];
    assert_eq!(io.read_at(reopened, 4, &mut bytes).unwrap(), 8);
    assert_eq!(&bytes, b"hello\0\0\0");
    assert_eq!(io.read_at(file, 32, &mut bytes).unwrap(), 0);
    let renamed = dir.0.join("renamed");
    io.rename(&path, &renamed).unwrap();
    assert_eq!(io.list(&dir.0).unwrap(), vec![renamed.clone()]);
    io.set_len(file, 5).unwrap();
    assert_eq!(io.len(reopened).unwrap(), 5);
    io.remove(&renamed).unwrap();
    assert!(!io.exists(&renamed).unwrap());
    assert!(matches!(io.open(&renamed), Err(Error::Io(_, _))));
}

#[test]
fn exclusive_lock_reports_holder_and_releases() {
    let dir = TestDir::new();
    let path = dir.0.join("LOCK");
    let first = StdIo::new();
    let file = first.create(&path).unwrap();
    first.write_at(file, 0, b"PID 123 server local").unwrap();
    let guard = first.try_lock_exclusive(&path).unwrap();
    let second = StdIo::new();
    match second.try_lock_exclusive(&path) {
        Err(Error::DataDirLocked { holder }) => assert_eq!(holder, "PID 123 server local"),
        result => panic!("expected lock contention, got {result:?}"),
    }
    drop(guard);
    assert!(second.try_lock_exclusive(&path).is_ok());
}

#[test]
fn clocks_are_shared_and_monotonic() {
    let clock = Arc::new(SimClock::new(100));
    let shared: Arc<dyn Clock> = clock.clone();
    assert_eq!(shared.now_ns(), 100);
    clock.advance(42);
    assert_eq!(shared.now_ns(), 142);
    clock.advance(u64::MAX);
    assert_eq!(shared.now_ns(), u64::MAX);
    let system = SystemClock;
    let before = system.now_ns();
    assert!(before > 0);
    assert!(system.now_ns() >= before);
}

#[test]
fn closed_handles_fail_without_invalidating_other_opens() {
    let dir = TestDir::new();
    for io in [Box::new(StdIo::new()) as Box<dyn Io>, Box::new(SimDisk::new(0))] {
        let path = dir.0.join("closed");
        let file = io.create(&path).unwrap();
        io.write_at(file, 0, b"data").unwrap();
        let other = io.open(&path).unwrap();
        assert_ne!(file, other);
        io.close(file).unwrap();
        assert!(io.read_at(file, 0, &mut [0; 4]).is_err());
        assert!(io.write_at(file, 0, b"bad").is_err());
        assert!(io.len(file).is_err());
        assert!(io.set_len(file, 0).is_err());
        assert!(io.sync_data(file).is_err());
        assert!(io.close(file).is_err());
        assert_eq!(io.len(other).unwrap(), 4);
        io.remove(&path).unwrap();
        assert_eq!(io.len(other).unwrap(), 4);
        io.close(other).unwrap();
    }
}

#[test]
fn directory_exists_and_list_match_across_backends() {
    let dir = TestDir::new();
    let branch = dir.0.join("branch");
    let deeper = branch.join("deeper");
    std::fs::create_dir_all(&deeper).unwrap();
    for io in [Box::new(StdIo::new()) as Box<dyn Io>, Box::new(SimDisk::new(0))] {
        let direct = dir.0.join("direct");
        let leaf = deeper.join("leaf");
        for path in [&direct, &leaf] {
            let file = io.create(path).unwrap();
            io.close(file).unwrap();
        }
        assert!(io.exists(&dir.0).unwrap());
        assert!(io.exists(&branch).unwrap());
        assert!(io.exists(&deeper).unwrap());
        assert!(io.exists(&leaf).unwrap());
        assert!(!io.exists(&branch.join("missing")).unwrap());
        assert_eq!(io.list(&dir.0).unwrap(), vec![branch.clone(), direct.clone()]);
        assert_eq!(io.list(&branch).unwrap(), vec![deeper.clone()]);
        assert_eq!(io.list(&deeper).unwrap(), vec![leaf.clone()]);
        assert!(io.list(&leaf).is_err());
        assert!(io.list(&branch.join("missing")).is_err());
        io.remove(&direct).unwrap();
        io.remove(&leaf).unwrap();
    }
}
