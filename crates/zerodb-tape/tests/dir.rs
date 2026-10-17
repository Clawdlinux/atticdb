#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use zerodb_tape::dir::{atomic_install, DataDir};
use zerodb_tape::io::{Io, StdIo};
use zerodb_tape::simdisk::SimDisk;
use zerodb_tape::Error;

mod support;

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
    assert!(matches!(DataDir::open(io, "db", None, 1024), Err(Error::CorruptionDetected { .. })));
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
    let path =
        std::env::current_dir().unwrap().join(format!("target/dir-test-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let io = Arc::new(StdIo::new());
    let dir = DataDir::open(io.clone(), &path, Some("localhost:77"), 1024).unwrap();
    std::fs::write(path.join("MANIFEST"), b"invalid").unwrap();
    match DataDir::open(io, &path, None, 1024) {
        Err(Error::DataDirLocked { holder }) => {
            assert!(holder.contains(&std::process::id().to_string()));
            assert!(holder.contains("localhost:77"));
        }

        other => panic!("{other:?}"),
    }
    drop(dir);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn missing_manifest_cannot_reinitialize_existing_segments() {
    let io = Arc::new(SimDisk::new(1));
    let mut dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
    dir.prepare_next_segment(1024).unwrap();
    drop(dir);
    io.remove(Path::new("db/MANIFEST")).unwrap();
    assert!(matches!(DataDir::open(io, "db", None, 1024), Err(Error::CorruptionDetected { .. })));
}

#[test]
fn retry_persists_orphan_removal_after_directory_sync_failure() {
    for seed in 0..32 {
        let io = Arc::new(SimDisk::new(seed));
        let mut dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
        dir.prepare_next_segment(1024).unwrap();
        let id = dir.prepare_next_segment(1024).unwrap();
        let orphan = dir.segment_path(id);
        dir.install_manifest(&[1]).unwrap();
        drop(dir);
        io.fail_next_sync().unwrap();
        assert!(matches!(DataDir::open(io.clone(), "db", None, 1024), Err(Error::SyncFailed(_))));
        assert!(!io.exists(&orphan).unwrap());
        let _retry = DataDir::open(io.clone(), "db", None, 1024).unwrap();
        assert!(!io.crash().unwrap().exists(&orphan).unwrap(), "seed={seed}");
    }
}
