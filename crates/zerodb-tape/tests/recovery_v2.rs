#![allow(clippy::unwrap_used)]

mod support;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zerodb_tape::sim::{frame, PreparedBatch, SimClock, WriteRequest, WriterCore};
use zerodb_tape::{Durability, Error, Io, Kind, RecordInput, SimDisk, Tape, TapeOptions};

fn opts(size: u64) -> TapeOptions {
    TapeOptions { segment_size: size, ..TapeOptions::default() }
}
fn append(core: &mut WriterCore, count: usize) {
    let batch = PreparedBatch::new(
        vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![7; 8] }; count],
        Durability::Durable,
    )
    .unwrap();
    let (request, reply) = WriteRequest::new(batch);
    core.enqueue(request);
    core.step();
    reply.recv().unwrap().unwrap();
}
fn flip(disk: &SimDisk, segment: u64, offset: u64) {
    let path = format!("db/seg-{segment:016}.zdb");
    let file = disk.open(Path::new(&path)).unwrap();
    disk.flip_bit(file, offset).unwrap();
    disk.close(file).unwrap();
}

#[test]
fn later_synced_footer_proves_corruption_in_same_or_later_segment() {
    for size in [1024, 128] {
        for offset in [0, 4, 40, 48 + 28] {
            let disk = Arc::new(SimDisk::new(1));
            let (mut core, _) =
                WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(size))
                    .unwrap();
            append(&mut core, 1);
            append(&mut core, 1);
            append(&mut core, 1);
            drop(core);
            flip(&disk, 1, offset);
            assert!(
                matches!(
                    Tape::open(disk, Arc::new(SimClock::default()), "db", opts(size)),
                    Err(Error::CorruptionDetected { lsn: 1, .. })
                ),
                "size={size} offset={offset}"
            );
        }
    }
}

#[test]
fn unproved_damage_truncates_and_reports() {
    let disk = Arc::new(SimDisk::new(2));
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024)).unwrap();
    append(&mut core, 1);
    drop(core);
    flip(&disk, 1, 40);
    let (_, report) = Tape::open(disk, Arc::new(SimClock::default()), "db", opts(1024)).unwrap();
    assert_eq!(report.durable_lsn, 0);
    assert_eq!(report.truncated.unwrap(), (1..=1, 1024));
}

#[test]
fn operator_cut_rejects_interior_lsns_with_both_boundaries() {
    let disk = Arc::new(SimDisk::new(3));
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024)).unwrap();
    append(&mut core, 1);
    append(&mut core, 3);
    core.shutdown().unwrap();
    drop(core);
    let error = Tape::recover_truncate_at_with(disk, "db", 2).unwrap_err().to_string();
    assert!(error.contains("1") && error.contains("4") && error.contains("transaction"), "{error}");
}

#[test]
fn operator_cut_at_boundary_reopens_above_and_below_superblock() {
    for checkpoint in [false, true] {
        for cut in [0, 1, 4] {
            let disk = Arc::new(SimDisk::new(4));
            let (mut core, _) =
                WriterCore::open(disk.clone(), Arc::new(SimClock::new(100)), "db", opts(256))
                    .unwrap();
            append(&mut core, 1);
            append(&mut core, 3);
            append(&mut core, 1);
            if checkpoint {
                core.shutdown().unwrap();
            }
            drop(core);
            let report = Tape::recover_truncate_at_with(disk.clone(), "db", cut).unwrap();
            assert_eq!(report.cut_lsn, Some(cut));
            assert!(report.lsns_may_be_reused);
            assert!(report.dropped_bytes > 0);
            assert_eq!(report.truncated.as_ref().unwrap().0, cut + 1..=5);
            let (mut core, reopened) =
                WriterCore::open(disk, Arc::new(SimClock::new(0)), "db", opts(256)).unwrap();
            assert_eq!(reopened.durable_lsn, cut);
            append(&mut core, 1);
            assert_eq!(core.durable_lsn(), cut + 1);
            assert_eq!(
                core.read(cut + 1).unwrap().unwrap().header.commit_time_ns,
                match cut {
                    0 => 1,
                    1 => 101,
                    _ => 102,
                },
            );
        }
    }
}

#[test]
fn operator_cut_validates_the_entire_kept_prefix() {
    let disk = Arc::new(SimDisk::new(5));
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024)).unwrap();
    append(&mut core, 1);
    append(&mut core, 1);
    core.shutdown().unwrap();
    drop(core);
    flip(&disk, 1, 40);
    assert!(matches!(
        Tape::recover_truncate_at_with(disk.clone(), "db", 2),
        Err(Error::CorruptionDetected { lsn: 1, .. })
    ));
    Tape::recover_truncate_at_with(disk, "db", 0).unwrap();
}

#[test]
fn every_operator_cut_crash_image_reopens_without_a_missing_durable_prefix() {
    let io = Arc::new(support::ObservedIo::new(6));
    let (mut core, _) =
        WriterCore::open(io.clone(), Arc::new(SimClock::default()), "db", opts(128)).unwrap();
    for _ in 0..3 {
        append(&mut core, 1);
    }
    core.shutdown().unwrap();
    drop(core);
    io.events.lock().unwrap().clear();
    io.capture.store(true, Ordering::SeqCst);
    Tape::recover_truncate_at_with(io.clone(), "db", 1).unwrap();
    let events = io.events.lock().unwrap();
    let data_synced =
        events.iter().position(|event| event == "sync db/seg-0000000000000001.zdb").unwrap();
    let floor_lowered =
        events.iter().position(|event| event == "rename db/SUPERBLOCK.tmp db/SUPERBLOCK").unwrap();
    assert!(data_synced < floor_lowered);
    let images = io.images.lock().unwrap();
    for (index, image) in images.iter().enumerate() {
        let (_, report) =
            Tape::open(Arc::new(image.clone()), Arc::new(SimClock::default()), "db", opts(128))
                .unwrap_or_else(|error| panic!("image {index} {}: {error}", events[index]));
        assert!(report.durable_lsn == 1 || report.durable_lsn == 3);
        if index >= data_synced {
            assert_eq!(report.durable_lsn, 1);
        }
    }
}

#[test]
fn valid_v2_footer_size_is_fixed() {
    assert_eq!(frame::HEADER_LEN, 40);
}

#[test]
fn a_bad_footer_is_expected_at_its_last_record_lsn() {
    for field in [8, 20, 40 + 8, 40 + 16, 40 + 24, 40 + 28] {
        let disk = Arc::new(SimDisk::new(7));
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024))
                .unwrap();
        append(&mut core, 3);
        append(&mut core, 1);
        drop(core);
        flip(&disk, 1, 3 * 48 + field);
        assert!(
            matches!(
                Tape::open(disk, Arc::new(SimClock::default()), "db", opts(1024)),
                Err(Error::CorruptionDetected { lsn: 3, .. })
            ),
            "field={field}"
        );
    }
}

#[test]
fn lookahead_requires_a_whole_valid_transaction_not_just_a_footer_crc() {
    for field in [0, 8, 16, 24, 28] {
        let disk = Arc::new(SimDisk::new(8));
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024))
                .unwrap();
        append(&mut core, 1);
        append(&mut core, 1);
        drop(core);
        let path = Path::new("db/seg-0000000000000001.zdb");
        let file = disk.open(path).unwrap();
        let mut bytes = vec![0; 1024];
        disk.read_at(file, 0, &mut bytes).unwrap();
        bytes[40] ^= 1;
        let footer = 120 + 48;
        bytes[footer + 40 + field] ^= if field == 16 { 2 } else { 1 };
        let crc = frame::checksum(1, footer as u64, &bytes[footer + 4..footer + 72]);
        bytes[footer..footer + 4].copy_from_slice(&crc.to_le_bytes());
        disk.write_at(file, 0, &bytes).unwrap();
        disk.sync_data(file).unwrap();
        disk.close(file).unwrap();
        let (_, report) =
            Tape::open(disk, Arc::new(SimClock::default()), "db", opts(1024)).unwrap();
        assert_eq!(report.durable_lsn, 0, "field={field}");
        assert_eq!(report.truncated.unwrap().0, 1..=2);
    }
}

#[test]
fn a_zeroed_footer_still_uses_the_last_pending_lsn_for_lookahead() {
    let disk = Arc::new(SimDisk::new(9));
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024)).unwrap();
    append(&mut core, 3);
    append(&mut core, 1);
    drop(core);
    let file = disk.open(Path::new("db/seg-0000000000000001.zdb")).unwrap();
    disk.write_at(file, 3 * 48, &[0; 72]).unwrap();
    disk.sync_data(file).unwrap();
    disk.close(file).unwrap();
    assert!(matches!(
        Tape::open(disk, Arc::new(SimClock::default()), "db", opts(1024)),
        Err(Error::CorruptionDetected { lsn: 3, .. })
    ));
}

#[test]
fn operator_cut_reports_the_next_lsn_for_an_unexpected_empty_footer() {
    let disk = Arc::new(SimDisk::new(10));
    let mut dir = zerodb_tape::sim::DataDir::open(disk.clone(), "db", None, 1024).unwrap();
    let segment = dir.prepare_next_segment(1024).unwrap();
    let footer = frame::Frame {
        header: frame::Header {
            kind: Kind::Footer,
            version: 2,
            flags: 0,
            txn: 1,
            lsn: 1,
            commit_time_ns: 1,
            tenant_id: 0,
        },
        payload: frame::Footer {
            txn: 1,
            first_lsn: 1,
            synced_lsn: 0,
            record_count: 1,
            records_crc: 0,
        }
        .encode()
        .to_vec(),
    };
    let bytes = frame::encode(&footer, segment, 0).unwrap();
    let file = disk.open(&dir.segment_path(segment)).unwrap();
    disk.write_at(file, 0, &bytes).unwrap();
    disk.sync_data(file).unwrap();
    disk.close(file).unwrap();
    drop(dir);
    assert!(matches!(
        Tape::recover_truncate_at_with(disk, "db", 1),
        Err(Error::CorruptionDetected { lsn: 1, .. })
    ));
}
