#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use zerodb_tape::sim::DataDir;
use zerodb_tape::{Error, Io, SimDisk};

#[test]
fn allocation_reserves_capacity_and_space_can_return() {
    let disk = SimDisk::new(1);
    disk.set_capacity(8192).unwrap();
    let file = disk.create(Path::new("db/file")).unwrap();
    assert_eq!(disk.total_bytes(Path::new("db")).unwrap(), 8192);
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 8192);
    disk.allocate(file, 4096).unwrap();
    assert_eq!(disk.len(file).unwrap(), 4096);
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 4096);
    disk.write_at(file, 0, &[7; 4096]).unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 4096);
    assert!(matches!(
        disk.allocate(file, 9000),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(disk.len(file).unwrap(), 4096);
    disk.set_capacity(16384).unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 12288);
    disk.allocate(file, 9000).unwrap();
    disk.set_len(file, 0).unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 16384);
}

#[test]
fn failed_allocation_does_not_change_manifest() {
    let disk = Arc::new(SimDisk::new(2));
    let mut dir = DataDir::open(disk.clone(), "db", None, 4096).unwrap();
    dir.prepare_next_segment(4096).unwrap();
    let before = dir.segments().to_vec();
    let free = disk.available_bytes(Path::new("db")).unwrap();
    let total = disk.total_bytes(Path::new("db")).unwrap();
    disk.set_capacity(total - free + 4095).unwrap();
    assert!(matches!(
        dir.prepare_next_segment(4096),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(dir.segments(), before);
    drop(dir);
    disk.set_capacity(total).unwrap();
    let dir = DataDir::open(disk, "db", None, 4096).unwrap();
    assert_eq!(dir.segments(), before);
}
