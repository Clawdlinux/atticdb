#![allow(clippy::unwrap_used)]

use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use zerodb_tape::io::{FileId, Io};
use zerodb_tape::simdisk::SimDisk;
use zerodb_tape::Error;

fn bytes(io: &dyn Io, file: FileId) -> Vec<u8> {
    let mut data = vec![0; io.len(file).unwrap() as usize];
    let count = io.read_at(file, 0, &mut data).unwrap();
    assert_eq!(count, data.len());
    data
}

fn durable_file(disk: &SimDisk, name: &str, data: &[u8]) -> FileId {
    let file = disk.create(Path::new(name)).unwrap();
    disk.write_at(file, 0, data).unwrap();
    disk.sync_data(file).unwrap();
    disk.sync_dir(Path::new("db")).unwrap();
    file
}

#[test]
fn synced_data_survives_1000_crash_seeds() {
    for seed in 0..1000 {
        let disk = SimDisk::new(seed);
        let file = durable_file(&disk, "db/data", b"durable");
        disk.write_at(file, 7, b"unsynced tail").unwrap();
        let crashed = disk.crash().unwrap();
        let file = crashed.open(Path::new("db/data")).unwrap();
        assert!(bytes(&crashed, file).starts_with(b"durable"), "seed {seed}");
    }
}

#[test]
fn unsynced_writes_are_lost_torn_and_retained() {
    let mut outcomes = [false; 3];
    for seed in 0..1000 {
        let disk = SimDisk::new(seed);
        let file = durable_file(&disk, "db/data", b"");
        disk.write_at(file, 0, b"pending").unwrap();
        let crashed = disk.crash().unwrap();
        match bytes(&crashed, file).len() {
            0 => outcomes[0] = true,
            7 => outcomes[2] = true,
            _ => outcomes[1] = true,
        }
    }
    assert_eq!(outcomes, [true; 3]);
}

#[test]
fn rename_crashes_are_atomic_for_1000_seeds() {
    let mut names = [false; 2];
    for seed in 0..1000 {
        let disk = SimDisk::new(seed);
        durable_file(&disk, "db/old", b"contents");
        disk.rename(Path::new("db/old"), Path::new("db/new")).unwrap();
        let crashed = disk.crash().unwrap();
        let old = crashed.exists(Path::new("db/old")).unwrap();
        let new = crashed.exists(Path::new("db/new")).unwrap();
        assert_ne!(old, new, "seed {seed}");
        names[usize::from(new)] = true;
    }
    assert_eq!(names, [true; 2]);
}

#[test]
fn failed_sync_keeps_pending_data() {
    let mut lost = false;
    for seed in 0..1000 {
        let disk = SimDisk::new(seed);
        let file = durable_file(&disk, "db/data", b"old");
        disk.write_at(file, 0, b"new").unwrap();
        disk.fail_next_sync().unwrap();
        assert!(disk.sync_data(file).is_err());
        assert_eq!(bytes(&disk, file), b"new");
        disk.sync_data(file).unwrap();
        lost |= bytes(&disk.crash().unwrap(), file) != b"new";
    }
    assert!(lost, "retrying a failed sync must not heal its writes");
}

#[test]
fn later_writes_do_not_resurrect_failed_sync_bytes() {
    let disk = SimDisk::new(11);
    let file = durable_file(&disk, "db/data", b"oldold");
    disk.write_at(file, 0, b"bad").unwrap();
    disk.fail_next_sync().unwrap();
    assert!(disk.sync_data(file).is_err());
    disk.sync_data(file).unwrap();
    disk.write_at(file, 3, b"new").unwrap();
    disk.sync_data(file).unwrap();
    assert_eq!(bytes(&disk, file), b"badnew");
    assert_eq!(bytes(&disk.crash().unwrap(), file), b"oldnew");
}

#[test]
fn same_seed_replays_the_same_image() {
    fn run(seed: u64) -> Vec<u8> {
        let disk = SimDisk::new(seed);
        let file = durable_file(&disk, "db/data", b"base");
        for offset in 0..20 {
            disk.write_at(file, offset, &[offset as u8; 8]).unwrap();
        }
        bytes(&disk.crash().unwrap(), file)
    }
    for seed in 0..1000 {
        assert_eq!(run(seed), run(seed), "seed {seed}");
    }
}

#[test]
fn unsynced_overwrites_can_reorder() {
    let mut reordered = false;
    for seed in 0..1000 {
        let disk = SimDisk::new(seed);
        let file = durable_file(&disk, "db/data", b"0000");
        disk.write_at(file, 0, b"1111").unwrap();
        disk.write_at(file, 0, b"2222").unwrap();
        let result = bytes(&disk.crash().unwrap(), file);
        reordered |= result == b"1122";
    }
    assert!(reordered);
}

#[test]
fn file_and_directory_sync_are_independent() {
    let mut lost = false;
    let mut retained = false;
    for seed in 0..1000 {
        let disk = SimDisk::new(seed);
        let file = disk.create(Path::new("db/data")).unwrap();
        disk.write_at(file, 0, b"data").unwrap();
        disk.sync_data(file).unwrap();
        let crashed = disk.crash().unwrap();
        let present = crashed.exists(Path::new("db/data")).unwrap();
        lost |= !present;
        retained |= present;
        if present {
            assert_eq!(bytes(&crashed, file), b"data");
        }
    }
    assert!(lost && retained);
}

#[test]
fn enospc_writes_a_prefix_and_bit_flips_damage_durable_data() {
    let disk = SimDisk::new(9);
    let file = durable_file(&disk, "db/data", b"");
    disk.enospc_after_bytes(3).unwrap();
    assert!(matches!(disk.write_at(file, 0, b"abcdef"), Err(Error::Io(ErrorKind::StorageFull, _))));
    assert_eq!(bytes(&disk, file), b"abc");
    disk.sync_data(file).unwrap();
    disk.flip_bit(file, 1).unwrap();
    assert_eq!(bytes(&disk.crash().unwrap(), file), b"acc");
}

#[test]
fn handles_lengths_directory_scope_and_locks() {
    let disk = Arc::new(SimDisk::new(19));
    let io: Arc<dyn Io> = disk.clone();
    let file = io.create(Path::new("db/data")).unwrap();
    assert!(io.create(Path::new("db/data")).is_err());
    io.set_len(file, 16).unwrap();
    io.write_at(file, 4, b"four").unwrap();
    io.sync_data(file).unwrap();
    io.sync_dir(Path::new("db")).unwrap();
    assert_eq!(bytes(&disk.crash().unwrap(), file).len(), 16);
    io.set_len(file, 6).unwrap();
    io.sync_data(file).unwrap();
    assert_eq!(bytes(&disk.crash().unwrap(), file), b"\0\0\0\0fo");
    assert_eq!(io.read_at(file, 100, &mut [0; 4]).unwrap(), 0);
    let guard = io.try_lock_exclusive(Path::new("db/LOCK")).unwrap();
    assert!(matches!(
        io.try_lock_exclusive(Path::new("db/LOCK")),
        Err(Error::DataDirLocked { .. })
    ));
    drop(guard);
    assert!(io.try_lock_exclusive(Path::new("db/LOCK")).is_ok());
    io.remove(Path::new("db/data")).unwrap();
    assert_eq!(bytes(io.as_ref(), file), b"\0\0\0\0fo");
    io.sync_dir(Path::new("db")).unwrap();
    assert!(!disk.crash().unwrap().exists(Path::new("db/data")).unwrap());
}

#[test]
fn destination_directory_sync_persists_cross_directory_rename() {
    let disk = SimDisk::new(31);
    let file = disk.create(Path::new("source/data")).unwrap();
    disk.write_at(file, 0, b"contents").unwrap();
    disk.sync_data(file).unwrap();
    disk.rename(Path::new("source/data"), Path::new("db/data")).unwrap();
    disk.sync_dir(Path::new("db")).unwrap();
    let crashed = disk.crash().unwrap();
    let file = crashed.open(Path::new("db/data")).unwrap();
    assert_eq!(bytes(&crashed, file), b"contents");
}

#[test]
fn rename_preserves_the_locked_inode() {
    let disk = SimDisk::new(0);
    let guard = disk.try_lock_exclusive(Path::new("db/LOCK")).unwrap();
    disk.rename(Path::new("db/LOCK"), Path::new("db/MOVED")).unwrap();
    assert!(matches!(
        disk.try_lock_exclusive(Path::new("db/MOVED")),
        Err(Error::DataDirLocked { .. })
    ));
    drop(guard);
    assert!(disk.try_lock_exclusive(Path::new("db/MOVED")).is_ok());
}
