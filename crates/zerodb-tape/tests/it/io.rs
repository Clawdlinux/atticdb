use crate::support::TestDir;
use std::sync::Arc;
use zerodb_tape::sim::SimClock;
use zerodb_tape::Error;
use zerodb_tape::{Clock, Io, SimDisk, StdIo, SystemClock};

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
fn allocation_sets_length_for_growth_repeats_and_shrink() {
    let dir = TestDir::new();
    #[cfg(target_os = "linux")]
    let preallocation_supported = crate::support::preallocation_supported(&dir.0);
    let io = StdIo::new();
    let path = dir.0.join("resized");
    let file = io.create(&path).unwrap();
    let metadata_file = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
    for (case, len, expect_reserved) in [
        ("empty file", 0, false),
        ("initial reservation", 1024 * 1024, true),
        ("repeated reservation", 1024 * 1024, false),
        ("shrink", 512 * 1024, false),
        ("growth", 2 * 1024 * 1024, false),
        ("repeated growth", 2 * 1024 * 1024, false),
        ("second growth", 3 * 1024 * 1024, false),
        ("truncate", 0, false),
    ] {
        eprintln!("case: {case}");
        #[cfg(target_os = "macos")]
        let before = {
            metadata_file.sync_all().unwrap();
            metadata_file.metadata().unwrap()
        };
        io.allocate(file, len).unwrap();
        assert_eq!(io.len(file).unwrap(), len, "{case}: length={len}");
        if expect_reserved {
            io.sync_data(file).unwrap();
        }
        metadata_file.sync_all().unwrap();
        let metadata = metadata_file.metadata().unwrap();
        assert_eq!(metadata.len(), len, "{case}: length={len}");
        // APFS reported 704 to 1696 of 2048 blocks for a 1 MiB F_PREALLOCATE when other tests
        // allocated in parallel, and all 2048 in isolation. Only Linux checks the lower bound.
        #[cfg(target_os = "linux")]
        if expect_reserved && preallocation_supported {
            use std::os::unix::fs::MetadataExt;
            assert!(metadata.blocks() * 512 >= len, "{case}: length={len}");
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::MetadataExt;
            let used = metadata.blocks() * 512;
            if len != 0 && len <= before.len() {
                assert!(
                    used < before.len() + len / 2,
                    "{case}: length={len} must not reserve a whole extra length"
                );
            }
            // Growth must not reserve the target length again past the physical end.
            assert!(
                used <= len.max(before.blocks() * 512) + len / 8,
                "{case}: length={len} used={used} over-reserves"
            );
        }
    }
    io.close(file).unwrap();
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
    for (case, io) in [
        ("filesystem", Box::new(StdIo::new()) as Box<dyn Io>),
        ("simulated", Box::new(SimDisk::new(0)) as Box<dyn Io>),
    ] {
        eprintln!("case: {case}");
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
    for (case, io) in [
        ("filesystem", Box::new(StdIo::new()) as Box<dyn Io>),
        ("simulated", Box::new(SimDisk::new(0)) as Box<dyn Io>),
    ] {
        eprintln!("case: {case}");
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

mod directory {
    use std::path::Path;
    use std::sync::Arc;
    use zerodb_tape::sim::{atomic_install, make_spare, DataDir};
    use zerodb_tape::Error;
    use zerodb_tape::{Io, SimDisk, StdIo};

    use crate::support;

    #[test]
    fn metadata_roundtrip_and_crc() {
        let io = Arc::new(SimDisk::new(1));
        let mut dir = DataDir::open(io.clone(), "db", Some("localhost:77"), 1024).unwrap();
        dir.install_manifest(&[1, 2]).unwrap();
        dir.install_superblock(99).unwrap();
        drop(dir);
        let dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
        assert_eq!(dir.segments(), &[1, 2]);
        assert_eq!(dir.superblock(), 99);
        assert_eq!(dir.segment_size(), 1024);
        drop(dir);
        let file = io.open(Path::new("db/MANIFEST")).unwrap();
        let mut prefix = [0; 20];
        io.read_at(file, 0, &mut prefix).unwrap();
        assert_eq!(prefix[8..12], 2u32.to_le_bytes());
        assert_eq!(prefix[12..20], 1024u64.to_le_bytes());
        io.flip_bit(file, 12).unwrap();
        io.close(file).unwrap();
        assert!(matches!(
            DataDir::open(io, "db", None, 1024),
            Err(Error::CorruptionDetected { .. })
        ));
    }

    #[test]
    fn atomic_install_is_old_or_new_at_every_crash_point() {
        for seed in 0..100 {
            let io = Arc::new(support::ObservedIo::new(seed));
            let _dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
            atomic_install(io.clone(), Path::new("db"), "TEST", b"old").unwrap();
            io.capture.store(true, std::sync::atomic::Ordering::SeqCst);
            atomic_install(io.clone(), Path::new("db"), "TEST", b"new-content").unwrap();
            assert!(io.handles.lock().unwrap().is_empty());
            for crash in io.images.lock().unwrap().iter() {
                let file = crash.open(Path::new("db/TEST")).unwrap();
                let mut bytes = vec![0; crash.len(file).unwrap() as usize];
                crash.read_at(file, 0, &mut bytes).unwrap();
                crash.close(file).unwrap();
                assert!(bytes == b"old" || bytes == b"new-content");
            }
        }
    }

    #[test]
    fn lock_is_first_and_reports_the_holder() {
        let dir = crate::support::TestDir::new();
        let path = &dir.0;
        let io = Arc::new(StdIo::new());
        let dir = DataDir::open(io.clone(), path, Some("localhost:77"), 1024).unwrap();
        std::fs::write(path.join("MANIFEST"), b"invalid").unwrap();
        match DataDir::open(io, path, None, 1024) {
            Err(Error::DataDirLocked { holder }) => {
                assert!(holder.contains(&std::process::id().to_string()));
                assert!(holder.contains("localhost:77"));
            }

            other => panic!("{other:?}"),
        }
        drop(dir);
    }

    #[test]
    fn missing_manifest_cannot_reinitialize_existing_segments() {
        let io = Arc::new(SimDisk::new(1));
        let mut dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
        dir.prepare_next_segment(1024).unwrap();
        drop(dir);
        io.remove(Path::new("db/MANIFEST")).unwrap();
        assert!(matches!(
            DataDir::open(io, "db", None, 1024),
            Err(Error::CorruptionDetected { .. })
        ));
    }

    #[test]
    fn retry_persists_orphan_removal_after_directory_sync_failure() {
        let db = Path::new("db");
        for (case, renamed_spare) in [("allocated orphan", false), ("renamed spare orphan", true)] {
            for seed in 0..32 {
                let io = Arc::new(SimDisk::new(seed));
                let mut dir = DataDir::open(io.clone(), db, None, 1024).unwrap();
                dir.prepare_next_segment(1024).unwrap();
                if renamed_spare {
                    make_spare(io.clone(), db, 1024).unwrap();
                }
                let id = dir.prepare_next_segment(1024).unwrap();
                assert!(!io.exists(&db.join("spare.zdb")).unwrap(), "{case}: roll took the spare");
                dir.install_manifest(&[1]).unwrap();
                // A crash can also leave a ready spare and a partial fill.
                make_spare(io.clone(), db, 1024).unwrap();
                io.close(io.create(&db.join("spare.tmp")).unwrap()).unwrap();
                let leftovers = [dir.segment_path(id), db.join("spare.zdb"), db.join("spare.tmp")];
                drop(dir);
                io.fail_next_sync().unwrap();
                assert!(
                    matches!(DataDir::open(io.clone(), db, None, 1024), Err(Error::SyncFailed(_))),
                    "{case}"
                );
                assert!(leftovers.iter().all(|path| !io.exists(path).unwrap()), "{case}");
                let _retry = DataDir::open(io.clone(), db, None, 1024).unwrap();
                let crash = io.crash().unwrap();
                assert!(
                    leftovers.iter().all(|path| !crash.exists(path).unwrap()),
                    "{case} seed={seed}"
                );
            }
        }
    }
}
