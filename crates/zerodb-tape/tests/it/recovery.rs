use crate::support::boundary_fixtures::{append, damage, empty_footer, opts, Damage};
use crate::support::recovery_fixtures::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zerodb_tape::sim::frame::{self, Kind};
use zerodb_tape::sim::{recover, DataDir};
use zerodb_tape::sim::{SimClock, WriterCore};
use zerodb_tape::Error;
use zerodb_tape::Tape;
use zerodb_tape::{Io, SimDisk};

use crate::support;

#[test]
fn oversized_padding_respects_the_durable_boundary() {
    for (case, floor) in [("proved gap", 2), ("unproved tail", 0)] {
        let io = Arc::new(SimDisk::new(0));
        let size = (frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 44 + 1024) as u64;
        let mut dir = DataDir::open(io.clone(), "db", None, size).unwrap();
        for _ in 0..2 {
            dir.prepare_next_segment(size).unwrap();
        }
        let (first, _) = transaction(1, 1, (1, 0));
        let (last, _) = transaction(2, 2, (2, 0));
        write(&io, &dir, 1, 0, &first);
        write(&io, &dir, 2, 0, &last);
        dir.install_superblock(floor).unwrap();
        let result = recover(&mut dir, size);
        if floor == 2 {
            assert!(matches!(result, Err(Error::CorruptionDetected { .. })), "{case}");
        } else {
            assert_eq!(result.unwrap().report.records, 1, "{case}");
            assert_eq!(dir.segments(), &[1], "{case}");
            dir.install_superblock(1).unwrap();
            drop(dir);
            for reopen in 0..2 {
                let mut dir = DataDir::open(io.clone(), "db", None, size).unwrap();
                assert_eq!(
                    recover(&mut dir, size).unwrap().report.records,
                    1,
                    "{case}, reopen={reopen}"
                );
            }
        }
    }
}

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
fn footer_lookahead_separates_proved_corruption_from_tail_damage() {
    use Damage::*;
    enum Expected {
        Corruption { lsn: u64 },
        Truncated { durable_lsn: u64, range: std::ops::RangeInclusive<u64> },
    }
    use Expected::*;
    let mut cases = vec![
        (
            "unproved record damage".into(),
            1024,
            vec![1],
            Flip(40),
            Truncated { durable_lsn: 0, range: 1..=1 },
        ),
        (
            "zeroed multi-record footer".into(),
            1024,
            vec![3, 1],
            ZeroFooter(144),
            Corruption { lsn: 3 },
        ),
    ];
    for size in [128, 1024] {
        for offset in [0, 4, 40, 76] {
            cases.push((
                format!("proved damage: size={size}, offset={offset}"),
                size,
                vec![1, 1, 1],
                Flip(offset),
                Corruption { lsn: 1 },
            ));
        }
        for fault in [GarbageFooter, MisdirectedFooter] {
            cases.push((
                format!("replaced footer: size={size}, fault={fault:?}"),
                size,
                vec![1, 1],
                fault,
                Corruption { lsn: 2 },
            ));
        }
    }
    for field in [8, 20, 48, 56, 64, 68] {
        cases.push((
            format!("last pending LSN: field={field}"),
            1024,
            vec![3, 1],
            Flip(144 + field),
            Corruption { lsn: 3 },
        ));
    }
    for field in [0, 8, 16, 24, 28] {
        cases.push((
            format!("CRC-valid but invalid proof: field={field}"),
            1024,
            vec![1, 1],
            InvalidProof(field),
            Truncated { durable_lsn: 0, range: 1..=2 },
        ));
    }
    for (case, segment_size, txns, corrupt, expected) in cases {
        let disk = Arc::new(SimDisk::new(73));
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(segment_size))
                .unwrap();
        for count in txns {
            append(&mut core, count);
        }
        drop(core);
        damage(&disk, segment_size, corrupt, &case);
        let result = Tape::open(disk, Arc::new(SimClock::default()), "db", opts(segment_size));
        match expected {
            Corruption { lsn } => assert!(
                matches!(&result,
                Err(Error::CorruptionDetected { lsn: actual, .. }) if *actual == lsn),
                "{case}: {result:?}"
            ),
            Truncated { durable_lsn, range } => {
                let (_, report) = result.unwrap_or_else(|error| panic!("{case}: {error}"));
                assert_eq!(report.durable_lsn, durable_lsn, "{case}");
                assert_eq!(report.truncated, Some((range, segment_size)), "{case}");
            }
        }
    }
}

#[test]
fn operator_cut_rejects_invalid_targets_and_corrupt_prefixes() {
    for (case, txns, cut, fault, lsn, message) in [
        ("interior LSN", vec![1, 3], 2, 0, None, "transaction"),
        ("corrupt prefix", vec![1, 1], 2, 1, Some(1), ""),
        ("empty footer", vec![], 1, 2, Some(1), ""),
        ("zero gap", vec![1, 1], 3, 3, Some(3), ""),
        ("beyond log", vec![1, 1], 3, 0, None, "cut LSN 3 exceeds last valid cut point 2"),
    ] {
        let size = if fault == 3 {
            (frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 32 + 241) as u64
        } else {
            1024
        };
        let disk = Arc::new(SimDisk::new(74));
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts(size))
                .unwrap();
        for count in txns {
            append(&mut core, count);
        }
        core.shutdown().unwrap();
        drop(core);
        match fault {
            1 => damage(&disk, 1024, Damage::Flip(40), case),
            2 => empty_footer(&disk),
            3 => {
                let mut dir = DataDir::open(disk.clone(), "db", None, size).unwrap();
                dir.prepare_next_segment(size).unwrap();
            }
            _ => {}
        }
        let error = Tape::recover_truncate_at_with(disk.clone(), "db", cut).unwrap_err();
        if let Some(lsn) = lsn {
            assert!(
                matches!(&error, Error::CorruptionDetected { lsn: actual, .. } if *actual == lsn),
                "{case}: {error:?}"
            );
        } else {
            assert!(matches!(error, Error::InvalidFrame(_)), "{case}");
            assert!(error.to_string().contains(message), "{case}: {error}");
            if case == "interior LSN" {
                assert!(
                    error.to_string().contains("1") && error.to_string().contains("4"),
                    "{case}: {error}"
                );
            }
        }
        if fault == 1 {
            assert_eq!(
                Tape::recover_truncate_at_with(disk, "db", 0).unwrap().cut_lsn,
                Some(0),
                "{case}"
            );
        }
    }
}

#[test]
fn operator_cut_discards_oversized_tail_and_bounds_the_superblock() {
    for floor in [0, 1, 5] {
        for cut in [0, 1, 4] {
            let case = format!("floor={floor}, cut={cut}");
            let disk = Arc::new(SimDisk::new(76));
            let (mut core, _) =
                WriterCore::open(disk.clone(), Arc::new(SimClock::new(100)), "db", opts(256))
                    .unwrap();
            for count in [1, 3, 1] {
                append(&mut core, count);
            }
            drop(core);
            let mut dir = DataDir::open(disk.clone(), "db", None, 256).unwrap();
            dir.install_superblock(floor).unwrap();
            let tail = dir.prepare_next_segment(256).unwrap();
            let path = dir.segment_path(tail);
            let file = disk.open(&path).unwrap();
            disk.set_len(file, 257).unwrap();
            disk.sync_data(file).unwrap();
            disk.close(file).unwrap();
            drop(dir);
            let report = Tape::recover_truncate_at_with(disk.clone(), "db", cut).unwrap();
            assert_eq!(report.cut_lsn, Some(cut), "{case}");
            assert!(report.lsns_may_be_reused, "{case}");
            assert_eq!(report.truncated.as_ref().unwrap().0, cut + 1..=5, "{case}");
            let kept_bytes = match cut {
                0 => 0,
                1 => 120,
                _ => 256 + 216,
            };
            assert_eq!(report.dropped_bytes, 1025 - kept_bytes, "{case}");
            assert!(!disk.exists(&path).unwrap(), "{case}");
            let dir = DataDir::open(disk.clone(), "db", None, 256).unwrap();
            assert_eq!(dir.superblock(), floor.min(cut), "{case}");
            drop(dir);
            let (mut core, report) =
                WriterCore::open(disk, Arc::new(SimClock::default()), "db", opts(256)).unwrap();
            assert_eq!(report.durable_lsn, cut, "{case}");
            append(&mut core, 1);
            assert_eq!(core.durable_lsn(), cut + 1, "{case}");
            assert_eq!(
                core.read(cut + 1).unwrap().unwrap().header.commit_time_ns,
                match cut {
                    0 => 1,
                    1 => 101,
                    _ => 102,
                },
                "{case}"
            );
        }
    }
}
