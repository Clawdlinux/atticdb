#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use zerodb_tape::sim::{DataDir, PreparedBatch, SimClock, WriteRequest, WriterCore};
use zerodb_tape::{Durability, Error, Io, Kind, RecordInput, SimDisk, Tape, TapeOptions};

fn append(core: &mut WriterCore) {
    let batch = PreparedBatch::new(
        vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![7; 8] }],
        Durability::Durable,
    )
    .unwrap();
    let (request, reply) = WriteRequest::new(batch);
    core.enqueue(request);
    core.step();
    reply.recv().unwrap().unwrap();
}

#[test]
fn zero_gap_reports_the_first_missing_lsn_not_the_segment_first_lsn() {
    let disk = Arc::new(SimDisk::new(74));
    let opts = TapeOptions::default();
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone()).unwrap();
    append(&mut core);
    append(&mut core);
    drop(core);
    let mut dir = DataDir::open(disk.clone(), "db", None, opts.segment_size).unwrap();
    dir.prepare_next_segment(opts.segment_size).unwrap();
    drop(dir);
    assert!(matches!(
        Tape::recover_truncate_at_with(disk, "db", 3),
        Err(Error::CorruptionDetected { lsn: 3, .. })
    ));
}

#[test]
fn healthy_preallocated_final_tail_reports_a_cut_beyond_the_log() {
    let disk = Arc::new(SimDisk::new(75));
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", TapeOptions::default())
            .unwrap();
    append(&mut core);
    append(&mut core);
    drop(core);
    let error = Tape::recover_truncate_at_with(disk, "db", 3).unwrap_err();
    assert!(matches!(error, Error::InvalidFrame(_)), "{error}");
    assert!(error.to_string().contains("cut LSN 3 exceeds last valid cut point 2"), "{error}");
}

#[test]
fn an_oversized_discarded_segment_does_not_block_an_operator_cut() {
    let disk = Arc::new(SimDisk::new(76));
    let opts = TapeOptions { segment_size: 1024, ..TapeOptions::default() };
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone()).unwrap();
    append(&mut core);
    drop(core);
    let mut dir = DataDir::open(disk.clone(), "db", None, opts.segment_size).unwrap();
    let segment = dir.prepare_next_segment(opts.segment_size).unwrap();
    let file = disk.open(&dir.segment_path(segment)).unwrap();
    disk.set_len(file, opts.segment_size + 1).unwrap();
    disk.sync_data(file).unwrap();
    disk.close(file).unwrap();
    drop(dir);
    let report = Tape::recover_truncate_at_with(disk.clone(), "db", 1).unwrap();
    assert_eq!(report.dropped_bytes, (1024 - 120) + 1025);
    let (_, report) = Tape::open(disk, Arc::new(SimClock::default()), "db", opts).unwrap();
    assert_eq!(report.durable_lsn, 1);
}

#[test]
fn an_operator_cut_lowers_but_never_raises_the_superblock() {
    for floor in [0, 1] {
        let disk = Arc::new(SimDisk::new(77));
        let opts = TapeOptions { segment_size: 1024, ..TapeOptions::default() };
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone())
                .unwrap();
        for _ in 0..3 {
            append(&mut core);
        }
        drop(core);
        let mut dir = DataDir::open(disk.clone(), "db", None, opts.segment_size).unwrap();
        dir.install_superblock(floor).unwrap();
        drop(dir);
        Tape::recover_truncate_at_with(disk.clone(), "db", 2).unwrap();
        let dir = DataDir::open(disk.clone(), "db", None, opts.segment_size).unwrap();
        assert_eq!(dir.superblock(), floor);
        drop(dir);
        let (_, report) = Tape::open(disk, Arc::new(SimClock::default()), "db", opts).unwrap();
        assert_eq!(report.durable_lsn, 2);
    }
}
