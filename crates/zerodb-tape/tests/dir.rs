#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use zerodb_tape::dir::{atomic_install, DataDir};
use zerodb_tape::io::{Io, StdIo};
use zerodb_tape::simdisk::SimDisk;
use zerodb_tape::Error;

#[test]
fn metadata_roundtrip_and_crc() {
    let io = Arc::new(SimDisk::new(1));
    let mut dir = DataDir::open(io.clone(), "db", Some("localhost:77")).unwrap();
    dir.install_manifest(&[1, 2]).unwrap();
    dir.install_superblock(99).unwrap();
    drop(dir);
    let dir = DataDir::open(io.clone(), "db", None).unwrap();
    assert_eq!(dir.segments(), &[1, 2]);
    assert_eq!(dir.superblock(), 99);
    drop(dir);
    let file = io.open(Path::new("db/MANIFEST")).unwrap();
    io.flip_bit(file, 12).unwrap();
    io.close(file).unwrap();
    assert!(matches!(DataDir::open(io, "db", None), Err(Error::CorruptionDetected { .. })));
}

#[test]
fn atomic_install_is_old_or_new_at_every_crash_point() {
    for seed in 0..100 {
        for point in 0..5 {
            let io = Arc::new(SimDisk::new(seed));
            let _dir = DataDir::open(io.clone(), "db", None).unwrap();
            atomic_install(io.clone(), Path::new("db"), "TEST", b"old").unwrap();
            let file = io.create(Path::new("db/TEST.tmp")).unwrap();
            if point >= 1 {
                io.write_at(file, 0, b"new-content").unwrap();
            }
            if point >= 2 {
                io.sync_data(file).unwrap();
            }
            if point >= 3 {
                io.rename(Path::new("db/TEST.tmp"), Path::new("db/TEST")).unwrap();
            }
            if point >= 4 {
                io.sync_dir(Path::new("db")).unwrap();
            }
            io.close(file).unwrap();
            let crash = io.crash().unwrap();
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
    let dir = DataDir::open(io.clone(), &path, Some("localhost:77")).unwrap();
    std::fs::write(path.join("MANIFEST"), b"invalid").unwrap();
    match DataDir::open(io, &path, None) {
        Err(Error::DataDirLocked { holder }) => {
            assert!(holder.contains(&std::process::id().to_string()));
            assert!(holder.contains("localhost:77"));
        }
        other => panic!("{other:?}"),
    }
    drop(dir);
    std::fs::remove_dir_all(path).unwrap();
}
