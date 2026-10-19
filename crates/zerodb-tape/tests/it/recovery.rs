use crate::support::recovery_fixtures::*;
use std::sync::Arc;
use zerodb_tape::sim::frame::{self, Kind};
use zerodb_tape::sim::{recover, DataDir};
use zerodb_tape::Error;
use zerodb_tape::{Io, SimDisk};

use crate::support;

#[test]
fn clean_reopen_checks_dense_lsns() {
    let (io, mut dir) = setup();
    let (first, head) = transaction(1, 1, (1, 0));
    let (second, _) = transaction(2, 2, head);
    write(&io, &dir, 1, 0, &[first, second].concat());
    dir.install_superblock(2).unwrap();
    let recovered = recover(&mut dir, 1024).unwrap();
    assert_eq!(recovered.report.durable_lsn, 2);
    assert_eq!(recovered.report.records, 2);
    assert!(recovered.report.truncated.is_none());
}

#[test]
fn lsn_gaps_cannot_bypass_dense_recovery() {
    for superblock in [0, u64::MAX] {
        let (io, mut dir) = setup();
        let second = dir.prepare_next_segment(1024).unwrap();
        let (valid, _) = transaction(u64::MAX, 1, (1, 0));
        write(&io, &dir, 1, 0, &valid);
        dir.install_superblock(superblock).unwrap();
        let file = io.open(&dir.segment_path(second)).unwrap();
        io.set_len(file, 1025).unwrap();
        io.sync_data(file).unwrap();
        io.close(file).unwrap();
        let result = recover(&mut dir, 1024);
        if superblock == 0 {
            let report = result.unwrap().report;
            assert_eq!(report.durable_lsn, 0);
            assert!(report.truncated.is_some());
        } else {
            assert!(matches!(result, Err(Error::CorruptionDetected { lsn: 1, .. })));
        }
    }
}

#[test]
fn missing_footer_drops_whole_transaction_and_zeroes_tail() {
    let (io, mut dir) = setup();
    let (first, head) = transaction(1, 1, (1, 0));
    let (second, _) = transaction(2, 2, head);
    write(&io, &dir, 1, 0, &first);
    write(&io, &dir, 1, first.len() as u64, &second[..second.len() - 10]);
    let recovered = recover(&mut dir, 1024).unwrap();
    assert_eq!(recovered.report.durable_lsn, 1);
    assert_eq!(recovered.report.truncated.as_ref().unwrap().0, 2..=2);
    let crash = io.crash().unwrap();
    let file = crash.open(&dir.segment_path(1)).unwrap();
    let mut tail = vec![1; 1024 - first.len()];
    crash.read_at(file, first.len() as u64, &mut tail).unwrap();
    crash.close(file).unwrap();
    assert!(tail.iter().all(|&byte| byte == 0));
}

#[test]
fn damage_missing_segment_and_early_end_below_superblock_are_corruption() {
    for failure in 0..3 {
        let (io, mut dir) = setup();
        let (bytes, _) = transaction(1, 1, (1, 0));
        write(&io, &dir, 1, 0, &bytes);
        dir.install_superblock(1).unwrap();
        match failure {
            0 => {
                let file = io.open(&dir.segment_path(1)).unwrap();
                io.flip_bit(file, 105).unwrap();
                io.close(file).unwrap();
            }
            1 => io.remove(&dir.segment_path(1)).unwrap(),
            _ => write(&io, &dir, 1, 0, &[0; 8]),
        }
        assert!(matches!(recover(&mut dir, 1024), Err(Error::CorruptionDetected { .. })));
    }
}

#[test]
fn bad_footer_binding_is_not_replayed() {
    for (case, field) in [
        ("first LSN binding", 8),
        ("synced LSN equals the first record", 16),
        ("records checksum", 28),
    ] {
        let (io, mut dir) = setup();
        let (bytes, _) = transaction(1, 1, (1, 0));
        let (record, used) = frame::decode(&bytes, 1, 0).unwrap();
        let (mut end, _) = frame::decode(&bytes[used..], 1, used as u64).unwrap();
        end.payload[field] ^= 1;
        let bytes =
            [frame::encode(&record, 1, 0).unwrap(), frame::encode(&end, 1, used as u64).unwrap()]
                .concat();

        write(&io, &dir, 1, 0, &bytes);
        let result = recover(&mut dir, 1024).unwrap();
        assert_eq!(result.report.records, 0, "{case}");
        assert!(result.report.truncated.is_some(), "{case}");
    }
}

#[test]
fn nonincreasing_lsn_is_truncated_above_and_corruption_below_boundary() {
    for boundary in [0, 2] {
        let (io, mut dir) = setup();
        let (first, head) = transaction(1, 1, (1, 0));
        let (second, _) = transaction(1, 2, head);
        write(&io, &dir, 1, 0, &[first, second].concat());
        dir.install_superblock(boundary).unwrap();
        let result = recover(&mut dir, 1024);
        if boundary == 0 {
            assert_eq!(result.unwrap().report.records, 1);
        } else {
            assert!(matches!(result, Err(Error::CorruptionDetected { .. })));
        }
    }
}

#[test]
fn truncation_report_includes_discarded_later_segments() {
    let (io, mut dir) = setup();
    let (first, head) = transaction(1, 1, (1, 0));
    let (second, _) = transaction(2, 2, head);
    let (third, _) = transaction(3, 3, (2, 0));
    dir.prepare_next_segment(1024).unwrap();
    write(&io, &dir, 1, 0, &first);
    write(&io, &dir, 1, first.len() as u64, &second[..second.len() - 10]);
    write(&io, &dir, 2, 0, &third);
    let state = recover(&mut dir, 1024).unwrap();
    assert_eq!(state.report.truncated.unwrap().0, 2..=3);
}

#[test]
fn truncation_resumes_at_the_cut_and_reopens_twice_after_new_ack() {
    use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
    use zerodb_tape::{Committed, Durability, RecordInput, TapeOptions as Options};
    for count in [3, 5] {
        for empty in [false, true] {
            for offset_zero in [false, true] {
                if empty && !offset_zero {
                    continue;
                }
                let (io, mut dir) = setup();
                for _ in 1..count {
                    dir.prepare_next_segment(1024).unwrap();
                }
                let (first, _) = transaction(1, 1, (1, 0));
                write(&io, &dir, 1, 0, &first);
                dir.install_superblock(1).unwrap();
                let mut last = 1;
                let mut cut = 0;
                if !offset_zero {
                    let (prefix, _) = transaction(2, 2, (2, 0));
                    write(&io, &dir, 2, 0, &prefix);
                    cut = prefix.len() as u64;
                    last = 2;
                }
                if !empty {
                    write(&io, &dir, 2, cut, &[1; 8]);
                }
                for id in 3..=count {
                    let (bytes, _) = transaction(last + id - 2, id, (id, 0));
                    let file = io.open(&dir.segment_path(id)).unwrap();
                    io.write_at(file, 0, &bytes).unwrap();
                    io.close(file).unwrap();
                }
                drop(dir);
                let opts = Options { segment_size: 1024, ..Options::default() };
                let clock = Arc::new(SimClock::new(0));
                let (mut core, report) =
                    WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
                assert_eq!(report.durable_lsn, last, "count={count} empty={empty} cut={cut}");
                assert_eq!(report.segments, 2);
                assert!(report.truncated.is_some());
                let batch = PreparedBatch::new(
                    vec![RecordInput {
                        kind: Kind::Turn,
                        tenant_id: 1,
                        content: b"after repair".to_vec(),
                    }],
                    Durability::Durable,
                )
                .unwrap();
                let (request, reply) = WriteRequest::new(batch);
                core.enqueue(request);
                core.step();
                assert!(
                    matches!(reply.recv().unwrap().unwrap(), Committed::Durable { last_lsn, .. } if last_lsn == last + 1)
                );
                clock.advance(1_000_000_000);
                core.update_superblock().unwrap();
                drop(core);
                for _ in 0..2 {
                    let image = Arc::new(io.crash().unwrap());
                    let (core, report) =
                        WriterCore::open(image, clock.clone(), "db", opts.clone()).unwrap();
                    assert!(report.truncated.is_none());
                    assert_eq!(&core.read(1).unwrap().unwrap().payload, b"hello");
                    assert_eq!(&core.read(last + 1).unwrap().unwrap().payload, b"after repair");
                }
                let dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
                assert_eq!(dir.segments(), &[1, 2]);
                for id in 3..=count {
                    assert!(!io.exists(&dir.segment_path(id)).unwrap());
                }
            }
        }
    }
}

#[test]
fn oversized_zero_padding_before_durable_boundary_is_corruption() {
    let io = Arc::new(SimDisk::new(0));
    let size = (frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 44 + 1024) as u64;
    let mut dir = DataDir::open(io.clone(), "db", None, size).unwrap();
    dir.prepare_next_segment(size).unwrap();
    dir.prepare_next_segment(size).unwrap();
    let (first, _) = transaction(1, 1, (1, 0));
    let (last, _) = transaction(2, 2, (2, 0));
    write(&io, &dir, 1, 0, &first);
    write(&io, &dir, 2, 0, &last);
    dir.install_superblock(2).unwrap();
    assert!(matches!(recover(&mut dir, size), Err(Error::CorruptionDetected { .. })));
}

#[test]
fn oversized_padding_above_boundary_is_not_promoted_into_durable_tape() {
    let io = Arc::new(SimDisk::new(0));
    let size = (frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 44 + 1024) as u64;
    let mut dir = DataDir::open(io.clone(), "db", None, size).unwrap();
    dir.prepare_next_segment(size).unwrap();
    dir.prepare_next_segment(size).unwrap();
    let (first, _) = transaction(1, 1, (1, 0));
    let (last, _) = transaction(2, 2, (2, 0));
    write(&io, &dir, 1, 0, &first);
    write(&io, &dir, 2, 0, &last);
    let report = recover(&mut dir, size).unwrap().report;
    assert_eq!(report.records, 1);
    assert_eq!(dir.segments(), &[1]);
    dir.install_superblock(1).unwrap();
    drop(dir);
    for _ in 0..2 {
        let mut dir = DataDir::open(io.clone(), "db", None, size).unwrap();
        assert_eq!(recover(&mut dir, size).unwrap().report.records, 1);
    }
}

#[test]
fn manifest_is_durable_before_tail_files_are_removed() {
    use std::sync::atomic::Ordering;
    for fail_manifest in [false, true] {
        let io = Arc::new(support::ObservedIo::new(1));
        let mut dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
        for _ in 0..3 {
            dir.prepare_next_segment(1024).unwrap();
        }

        let (first, _) = transaction(1, 1, (1, 0));
        write(&io.disk, &dir, 1, 0, &first);
        dir.install_superblock(1).unwrap();
        write(&io.disk, &dir, 2, 0, &[1; 8]);
        io.events.lock().unwrap().clear();
        io.syncs.store(0, Ordering::SeqCst);
        io.capture.store(!fail_manifest, Ordering::SeqCst);
        if fail_manifest {
            io.fail_at.store(3, Ordering::SeqCst);
        }
        let result = recover(&mut dir, 1024);
        if fail_manifest {
            assert!(result.is_err());
            assert_eq!(dir.segments(), &[1, 2, 3]);
            assert!(io.exists(&dir.segment_path(3)).unwrap());
        } else {
            result.unwrap();
            let events = io.events.lock().unwrap();
            let installed = events.iter().position(|e| e == "dir-sync db").unwrap();
            let removed =
                events.iter().position(|e| e == "remove db/seg-0000000000000003.zdb").unwrap();
            assert!(installed < removed);
            for image in io.images.lock().unwrap().iter() {
                for _ in 0..2 {
                    let mut dir = DataDir::open(Arc::new(image.clone()), "db", None, 1024).unwrap();
                    assert_eq!(recover(&mut dir, 1024).unwrap().report.durable_lsn, 1);
                }
            }
        }
        drop(dir);
        assert!(io.handles.lock().unwrap().is_empty());
    }
}

#[test]
fn reopen_removes_orphans_after_manifest_trim_before_unlink() {
    use std::sync::atomic::Ordering;
    use zerodb_tape::sim::SimClock;
    use zerodb_tape::{Tape, TapeOptions as Options};

    let io = Arc::new(support::ObservedIo::new(1));
    let mut dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
    for _ in 0..3 {
        dir.prepare_next_segment(1024).unwrap();
    }
    let (first, _) = transaction(1, 1, (1, 0));
    write(&io.disk, &dir, 1, 0, &first);
    dir.install_superblock(1).unwrap();
    write(&io.disk, &dir, 2, 0, &[1; 8]);
    io.events.lock().unwrap().clear();
    io.capture.store(true, Ordering::SeqCst);
    recover(&mut dir, 1024).unwrap();
    let installed = io.events.lock().unwrap().iter().position(|e| e == "dir-sync db").unwrap();
    let image = io.images.lock().unwrap()[installed].clone();
    let orphan = dir.segment_path(3);
    assert!(image.exists(&orphan).unwrap());
    let image = Arc::new(image);
    let reopened = DataDir::open(image.clone(), "db", None, 1024).unwrap();
    assert!(!image.exists(&orphan).unwrap(), "orphan must disappear before recovery");
    drop(reopened);
    let crash = Arc::new(image.crash().unwrap());
    assert!(!crash.exists(&orphan).unwrap(), "orphan cleanup must be durable");
    let (tape, report) = Tape::open(
        crash,
        Arc::new(SimClock::default()),
        "db",
        Options { segment_size: 1024, ..Options::default() },
    )
    .unwrap();
    assert_eq!(report.durable_lsn, 1);
    assert_eq!(&tape.read(1).unwrap().unwrap().payload, b"hello");
}

mod boundaries {
    use crate::support;
    use crate::support::boundary_fixtures::*;
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use zerodb_tape::sim::{frame, SimClock, WriterCore};
    use zerodb_tape::{Error, Io, Kind, SimDisk, Tape};

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
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024))
                .unwrap();
        append(&mut core, 1);
        drop(core);
        flip(&disk, 1, 40);
        let (_, report) =
            Tape::open(disk, Arc::new(SimClock::default()), "db", opts(1024)).unwrap();
        assert_eq!(report.durable_lsn, 0);
        assert_eq!(report.truncated.unwrap(), (1..=1, 1024));
    }

    #[test]
    fn operator_cut_rejects_interior_lsns_with_both_boundaries() {
        let disk = Arc::new(SimDisk::new(3));
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024))
                .unwrap();
        append(&mut core, 1);
        append(&mut core, 3);
        core.shutdown().unwrap();
        drop(core);
        let error = Tape::recover_truncate_at_with(disk, "db", 2).unwrap_err().to_string();
        assert!(
            error.contains("1") && error.contains("4") && error.contains("transaction"),
            "{error}"
        );
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
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024))
                .unwrap();
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
        let floor_lowered = events
            .iter()
            .position(|event| event == "rename db/SUPERBLOCK.tmp db/SUPERBLOCK")
            .unwrap();
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
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(1024))
                .unwrap();
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
}

mod operator_cuts {
    use crate::support::boundary_fixtures::append;
    use std::sync::Arc;
    use zerodb_tape::sim::{DataDir, SimClock, WriterCore};
    use zerodb_tape::{Error, Io, SimDisk, Tape, TapeOptions};

    #[test]
    fn zero_gap_reports_the_first_missing_lsn_not_the_segment_first_lsn() {
        let disk = Arc::new(SimDisk::new(74));
        let opts = TapeOptions::default();
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone())
                .unwrap();
        append(&mut core, 1);
        append(&mut core, 1);
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
        let (mut core, _) = WriterCore::open(
            disk.clone(),
            Arc::new(SimClock::default()),
            "db",
            TapeOptions::default(),
        )
        .unwrap();
        append(&mut core, 1);
        append(&mut core, 1);
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
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone())
                .unwrap();
        append(&mut core, 1);
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
                append(&mut core, 1);
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
}

mod footer_damage {
    use std::path::Path;
    use std::sync::Arc;
    use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
    use zerodb_tape::{Durability, Error, Io, Kind, RecordInput, SimDisk, Tape, TapeOptions};

    #[test]
    fn damaged_footer_cannot_hide_a_later_synced_transaction() {
        for (case, misdirected) in [("garbage", false), ("misdirected", true)] {
            for size in [128, 1024] {
                let disk = Arc::new(SimDisk::new(73));
                let opts = TapeOptions { segment_size: size, ..TapeOptions::default() };
                let (mut core, _) = WriterCore::open(
                    disk.clone(),
                    Arc::new(SimClock::default()),
                    "db",
                    opts.clone(),
                )
                .unwrap();
                for value in [1, 2] {
                    let batch = PreparedBatch::new(
                        vec![RecordInput {
                            kind: Kind::Turn,
                            tenant_id: 1,
                            content: vec![value; 8],
                        }],
                        Durability::Durable,
                    )
                    .unwrap();
                    let (request, reply) = WriteRequest::new(batch);
                    core.enqueue(request);
                    core.step();
                    reply.recv().unwrap().unwrap();
                }
                drop(core);
                let first = disk.open(Path::new("db/seg-0000000000000001.zdb")).unwrap();
                if misdirected {
                    let (path, offset) = if size == 128 {
                        ("db/seg-0000000000000002.zdb", 0)
                    } else {
                        ("db/seg-0000000000000001.zdb", 120)
                    };
                    let second = disk.open(Path::new(path)).unwrap();
                    let mut frame = [0; 48];
                    assert_eq!(
                        disk.read_at(second, offset, &mut frame).unwrap(),
                        frame.len(),
                        "case={case}, segment_size={size}"
                    );
                    disk.close(second).unwrap();
                    disk.write_at(first, 48, &frame).unwrap();
                } else {
                    disk.write_at(first, 48, &[0xff; 72]).unwrap();
                }
                disk.sync_data(first).unwrap();
                disk.close(first).unwrap();
                assert!(
                    matches!(
                        Tape::open(disk, Arc::new(SimClock::default()), "db", opts),
                        Err(Error::CorruptionDetected { .. })
                    ),
                    "case={case}, segment_size={size}"
                );
            }
        }
    }
}
