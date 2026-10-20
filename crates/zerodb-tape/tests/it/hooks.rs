use crate::support::{opts, record, submit};
use crate::support::{receipt, Behavior, Machine};
use std::sync::{Arc, Barrier};
use zerodb_tape::sim::{SimClock, WriterCore};
use zerodb_tape::{
    Committed, Durability, Error, Kind, Outcome, Result, SimDisk, Tape, TapeOptions,
};

#[test]
fn writer_assigns_dense_sequences_and_publishes_before_reply_under_eight_threads() {
    let machine = Machine::new(Behavior::Sequence);
    let log = machine.log.clone();
    let (tape, _) = Tape::open_with(
        machine,
        Arc::new(SimDisk::new(1)),
        Arc::new(SimClock::default()),
        "db",
        opts(512),
    )
    .unwrap();
    let tape = Arc::new(tape);
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let tape = &tape;
            let barrier = &barrier;
            let log = &log;
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..16 {
                    let Committed::Durable { first_lsn, last_lsn, .. } = tape
                        .append(vec![record(Kind::Turn, 7, [0; 8]); 2], Durability::Durable)
                        .unwrap();
                    let log = log.lock().unwrap();
                    for lsn in first_lsn..=last_lsn {
                        assert_eq!(log.published.iter().filter(|r| r.lsn == lsn).count(), 1);
                    }
                }
            });
        }
    });
    let records = tape.scan(0).collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(records.len(), 256);
    for (i, record) in records.iter().enumerate() {
        assert_eq!(record.lsn, i as u64 + 1);
        assert_eq!(record.tenant_id, 7);
        assert_eq!(record.kind, Kind::Turn);
        assert_eq!(record.payload.as_ref(), &(i as u64 + 1).to_le_bytes());
    }
    let log = log.lock().unwrap();
    assert_eq!(log.published, records);
    assert_eq!(log.prepares, 128);
    assert!(log.threads.iter().all(|id| *id == log.threads[0]));
    assert_ne!(log.threads[0], std::thread::current().id());
}

#[test]
fn reject_and_cached_reply_write_nothing() {
    let machine = Machine::new(Behavior::Sequence);
    let log = machine.log.clone();
    let (tape, _) = Tape::open_with(
        machine,
        Arc::new(SimDisk::new(2)),
        Arc::new(SimClock::default()),
        "db",
        opts(512),
    )
    .unwrap();
    let error =
        tape.append(vec![record(Kind::Turn, 7, b"reject")], Durability::Durable).unwrap_err();
    assert_eq!(error.outcome, Outcome::NotCommitted);
    assert_eq!(
        tape.append(vec![record(Kind::Turn, 7, b"reply")], Durability::Durable).unwrap(),
        receipt(77, 88)
    );
    assert_eq!(tape.durable_lsn(), 0);
    assert_eq!(tape.scan(0).count(), 0);
    assert!(log.lock().unwrap().published.is_empty());
    assert!(matches!(
        tape.append(vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable).unwrap(),
        Committed::Durable { txn: 1, first_lsn: 1, last_lsn: 1, .. }
    ));
}

#[test]
fn durable_records_publish_each_step_and_shutdown_is_idempotent() {
    let machine = Machine::new(Behavior::Sequence);
    let log = machine.log.clone();
    let (mut core, _) = WriterCore::open_with(
        machine,
        Arc::new(SimDisk::new(3)),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { segment_size: 2048, ..opts(512) },
    )
    .unwrap();
    for _ in 0..3 {
        let reply = submit(&mut core, vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable);
        core.step();
        reply.recv().unwrap().unwrap();
    }
    assert_eq!(log.lock().unwrap().published.len(), 3);
    assert_eq!(core.scan(0).count(), 3);
    let reply = submit(&mut core, vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable);
    core.step();
    reply.recv().unwrap().unwrap();
    assert_eq!(
        log.lock().unwrap().published.iter().map(|r| r.lsn).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    let old_scan = core.scan(0).collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(old_scan.len(), 4);
    let reply = submit(&mut core, vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable);
    core.step();
    reply.recv().unwrap().unwrap();
    assert_eq!(core.scan(0).count(), 5);
    core.shutdown().unwrap();
    assert_eq!(log.lock().unwrap().published.len(), 5);
    core.shutdown().unwrap();
    assert_eq!(log.lock().unwrap().published.len(), 5);
}

#[test]
fn failed_sync_never_publishes_and_poison_hides_prepare_side_effects() {
    for seed in 0..64 {
        let disk = Arc::new(SimDisk::new(seed));
        let machine = Machine::new(Behavior::Sequence);
        let log = machine.log.clone();
        let (mut core, _) = WriterCore::open_with(
            machine,
            disk.clone(),
            Arc::new(SimClock::default()),
            "db",
            opts(512),
        )
        .unwrap();
        let first = submit(&mut core, vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable);
        let reply = submit(&mut core, vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable);
        disk.fail_next_sync().unwrap();
        core.step();
        assert_eq!(first.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
        assert!(matches!(reply.recv().unwrap().unwrap_err().kind, Error::SyncFailed(_)));
        assert_eq!(core.durable_lsn(), 0);
        assert!(log.lock().unwrap().published.is_empty());
        assert_eq!(core.scan(0).count(), 0);
        let reply = submit(&mut core, vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable);
        core.step();
        assert!(matches!(reply.recv().unwrap().unwrap_err().kind, Error::Poisoned));
        assert_eq!(log.lock().unwrap().prepares, 2);
        assert!(core.shutdown().is_err());
        assert!(log.lock().unwrap().published.is_empty());
    }
}

#[test]
fn replay_is_ordered_exclusive_of_watermark_and_finishes_before_writes() {
    let disk = Arc::new(SimDisk::new(4));
    let clock = Arc::new(SimClock::default());
    let (tape, _) = Tape::open_with(
        Machine::new(Behavior::Sequence),
        disk.clone(),
        clock.clone(),
        "db",
        opts(512),
    )
    .unwrap();
    for _ in 0..5 {
        tape.append(vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable).unwrap();
    }
    let all = tape.scan(0).collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(tape.scan(3).collect::<Result<Vec<_>>>().unwrap(), all[2..]);
    assert_eq!(tape.scan(6).count(), 0);
    assert_eq!(tape.scan(u64::MAX).count(), 0);
    {
        let snapshot = tape.scan(0);
        tape.append(vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable).unwrap();
        assert_eq!(snapshot.collect::<Result<Vec<_>>>().unwrap(), all);
    }
    drop(tape);
    for watermark in [0, 3, 6, u64::MAX] {
        eprintln!("case: watermark={watermark:?}");
        let machine = Machine { seq: watermark.min(6), ..Machine::new(Behavior::Sequence) };
        let log = machine.log.clone();
        let opts = TapeOptions { replay_from_lsn: watermark, ..opts(512) };
        let (tape, _) = Tape::open_with(machine, disk.clone(), clock.clone(), "db", opts).unwrap();
        assert_eq!(
            log.lock().unwrap().replayed.iter().map(|r| r.lsn).collect::<Vec<_>>(),
            (1..=6).filter(|lsn| *lsn > watermark).collect::<Vec<_>>()
        );
        assert!(log.lock().unwrap().published.is_empty());
        drop(tape);
    }
    let (tape, _) = Tape::open_with(
        Machine { seq: 3, ..Machine::new(Behavior::Sequence) },
        disk,
        clock,
        "db",
        TapeOptions { replay_from_lsn: 3, ..opts(512) },
    )
    .unwrap();
    tape.append(vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable).unwrap();
    assert_eq!(tape.scan(7).next().unwrap().unwrap().payload.as_ref(), &7u64.to_le_bytes());
}

#[test]
fn replay_error_aborts_open_and_releases_the_lock() {
    let disk = Arc::new(SimDisk::new(5));
    let clock = Arc::new(SimClock::default());
    let (tape, _) = Tape::open(disk.clone(), clock.clone(), "db", opts(512)).unwrap();
    tape.append(vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable).unwrap();
    drop(tape);
    assert!(matches!(
        Tape::open_with(
            Machine { fail_replay: true, ..Machine::new(Behavior::Sequence) },
            disk.clone(),
            clock.clone(),
            "db",
            opts(512)
        ),
        Err(Error::Configuration(_))
    ));
    Tape::open(disk, clock, "db", opts(512)).unwrap();
}

#[test]
fn failed_sync_survivors_replay_only_after_recovery_successfully_syncs() {
    for seed in 0..32 {
        let disk = Arc::new(SimDisk::new(seed));
        let clock = Arc::new(SimClock::default());
        let machine = Machine::new(Behavior::Sequence);
        let log = machine.log.clone();
        let (mut core, _) =
            WriterCore::open_with(machine, disk.clone(), clock.clone(), "db", opts(512)).unwrap();
        let reply = submit(&mut core, vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable);
        disk.fail_next_sync().unwrap();
        core.step();
        assert_eq!(reply.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
        assert!(log.lock().unwrap().published.is_empty());
        drop(core);

        let machine = Machine::new(Behavior::Sequence);
        let log = machine.log.clone();
        disk.fail_next_sync().unwrap();
        assert!(Tape::open_with(machine, disk.clone(), clock.clone(), "db", opts(512)).is_err());
        assert!(log.lock().unwrap().replayed.is_empty());

        let machine = Machine::new(Behavior::Sequence);
        let log = machine.log.clone();
        let (tape, report) =
            Tape::open_with(machine, disk.clone(), clock.clone(), "db", opts(512)).unwrap();
        assert_eq!(report.durable_lsn, 1);
        assert_eq!(log.lock().unwrap().replayed, tape.scan(0).collect::<Result<Vec<_>>>().unwrap());
        tape.append(vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable).unwrap();
        assert_eq!(tape.scan(2).next().unwrap().unwrap().payload.as_ref(), &2u64.to_le_bytes());
        drop(tape);
        let (tape, _) =
            Tape::open(Arc::new(disk.crash().unwrap()), clock, "db", opts(512)).unwrap();
        assert_eq!(tape.scan(0).count(), 2);
    }
}

#[test]
fn rewritten_batches_are_revalidated_without_exposing_reserved_state() {
    for (content, mode) in
        [(&b"footer"[..], Durability::Durable), (&b"too large"[..], Durability::Durable)]
    {
        eprintln!("case: content={content:?}, mode={mode:?}");
        let (tape, _) = Tape::open_with(
            Machine::new(Behavior::InvalidRewrite),
            Arc::new(SimDisk::new(9)),
            Arc::new(SimClock::default()),
            "db",
            opts(512),
        )
        .unwrap();
        let kind = Kind::Turn;
        assert_eq!(
            tape.append(vec![record(kind, 7, content)], mode).unwrap_err().outcome,
            Outcome::NotCommitted
        );
        assert_eq!(tape.durable_lsn(), 0);
        assert_eq!(tape.scan(0).count(), 0);
        assert!(matches!(
            tape.append(vec![record(Kind::Turn, 7, b"next")], Durability::Durable)
                .unwrap_err()
                .kind,
            Error::Poisoned
        ));
    }
}

#[test]
fn scan_skips_footers_and_handles_an_empty_maximum_lsn_range() {
    let disk = Arc::new(SimDisk::new(6));
    let (tape, _) = Tape::open(
        disk,
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { segment_size: 1024, ..TapeOptions::default() },
    )
    .unwrap();
    for _ in 0..3 {
        tape.append(vec![record(Kind::Turn, 7, b"hello")], Durability::Durable).unwrap();
    }
    let records = tape.scan(0).collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert!(records.iter().all(|r| r.payload.as_ref() == b"hello"));
    let mut scan = tape.scan(u64::MAX);
    assert!(scan.next().is_none());
    assert!(scan.next().is_none());
}

#[test]
fn scan_reports_corruption_and_stops_after_the_bad_frame() {
    use zerodb_tape::Io;
    let disk = Arc::new(SimDisk::new(7));
    let (tape, _) = Tape::open(
        disk.clone(),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { segment_size: 160, ..opts(512) },
    )
    .unwrap();
    for _ in 0..3 {
        tape.append(vec![record(Kind::Turn, 7, [0; 8])], Durability::Durable).unwrap();
    }
    let file = disk.open(std::path::Path::new("db/seg-0000000000000002.zdb")).unwrap();
    disk.flip_bit(file, 0).unwrap();
    disk.close(file).unwrap();
    let mut scan = tape.scan(0);
    assert_eq!(scan.next().unwrap().unwrap().lsn, 1);
    assert!(matches!(scan.next().unwrap(), Err(Error::CorruptionDetected { lsn: 2, .. })));
    assert!(scan.next().is_none());
}

mod retries {
    use std::sync::Arc;
    use zerodb_tape::sim::{SimClock, WriterCore};
    use zerodb_tape::{Error, Outcome, SimDisk, TapeOptions};

    use crate::support::{receipt, Behavior, Machine};
    use crate::support::{record, submit};
    use zerodb_tape::{Durability, Kind};

    #[test]
    fn grouped_retries_share_sync_publication_and_partial_write_outcomes() {
        for (case, partial, fail_sync) in [
            ("complete group", false, false),
            ("complete group sync failure", false, true),
            ("partial tail", true, false),
            ("partial tail sync failure", true, true),
        ] {
            eprintln!("case: {case}");
            let disk = Arc::new(SimDisk::new(1));
            let machine = Machine::new(Behavior::Dedup);
            let replies = machine.replies.clone();
            let published = machine.log.clone();
            let (mut core, _) = WriterCore::open_with(
                machine,
                disk.clone(),
                Arc::new(SimClock::default()),
                "db",
                TapeOptions { segment_size: 2048, ..TapeOptions::default() },
            )
            .unwrap();
            for _ in 0..2 {
                let reply = submit(
                    &mut core,
                    vec![record(Kind::Turn, 1, b"original")],
                    Durability::Durable,
                );
                replies.lock().unwrap().push(reply);
            }
            let trailing_reply = if partial {
                let reply = submit(
                    &mut core,
                    vec![record(Kind::Turn, 1, b"trailing")],
                    Durability::Durable,
                );
                disk.enospc_after_bytes(125).unwrap();
                Some(reply)
            } else {
                None
            };
            if fail_sync {
                disk.fail_next_sync().unwrap();
            }
            core.step();
            for reply in replies.lock().unwrap().iter() {
                let result = reply.recv().unwrap();
                if fail_sync {
                    let error = result.unwrap_err();
                    if !partial {
                        assert!(matches!(error.kind, Error::SyncFailed(_)), "{case}");
                    }
                    assert_eq!(error.outcome, Outcome::Unknown, "{case}");
                } else {
                    assert_eq!(result.unwrap(), receipt(1, 1), "{case}");
                }
            }
            if let Some(reply) = trailing_reply {
                assert!(reply.recv().unwrap().is_err(), "{case}");
            }
            assert_eq!(core.is_poisoned(), partial || fail_sync, "{case}");
            assert_eq!(
                published.lock().unwrap().published.iter().map(|r| r.lsn).collect::<Vec<_>>(),
                if fail_sync { Vec::new() } else { vec![1] },
                "{case}",
            );
        }
    }
}
