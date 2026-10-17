#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use zerodb_tape::dir::DataDir;
use zerodb_tape::io::Io;
use zerodb_tape::segment::SegmentWriter;
use zerodb_tape::simdisk::SimDisk;
use zerodb_tape::Error;

#[test]
fn roll_prepares_names_and_syncs_every_dirty_file() {
    for seed in 0..100 {
        let io = Arc::new(SimDisk::new(seed));
        let dir = DataDir::open(io.clone(), "db", None).unwrap();
        let mut writer = SegmentWriter::new(dir, 512, 0).unwrap();
        assert_eq!(writer.write_batch(&[7; 300]).unwrap(), (1, 0));
        assert_eq!(writer.write_batch(&[8; 300]).unwrap(), (2, 0));
        writer.sync().unwrap();
        drop(writer);
        let crash = io.crash().unwrap();
        let dir = DataDir::open(Arc::new(crash.clone()), "db", None).unwrap();
        assert_eq!(dir.segments(), &[1, 2]);
        for (id, byte) in [(1, 7), (2, 8)] {
            let file = crash.open(&dir.segment_path(id)).unwrap();
            assert_eq!(crash.len(file).unwrap(), 512);
            let mut bytes = [0; 512];
            crash.read_at(file, 0, &mut bytes).unwrap();
            crash.close(file).unwrap();
            assert_eq!(&bytes[..300], &[byte; 300]);
            assert_eq!(&bytes[300..], &[0; 212]);
        }
    }
}

#[test]
fn reject_oversized_transaction_without_rolling() {
    let io = Arc::new(SimDisk::new(0));
    let dir = DataDir::open(io.clone(), "db", None).unwrap();
    let mut writer = SegmentWriter::new(dir, 512, 0).unwrap();
    assert!(matches!(writer.write_batch(&[0; 513]), Err(Error::BatchTooLarge)));
    assert!(!io.exists(Path::new("db/seg-0000000000000002.zdb")).unwrap());
}

#[test]
fn preparation_failure_does_not_switch_manifest() {
    let io = Arc::new(SimDisk::new(0));
    let mut dir = DataDir::open(io.clone(), "db", None).unwrap();
    dir.prepare_next_segment(512).unwrap();
    io.fail_next_sync().unwrap();
    assert!(dir.prepare_next_segment(512).is_err());
    assert_eq!(dir.segments(), &[1]);
}
