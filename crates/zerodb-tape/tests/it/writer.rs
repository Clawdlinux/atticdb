use crate::support::writer_fixtures::*;
use std::sync::{Arc, Barrier};
use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
use zerodb_tape::Error;
use zerodb_tape::{
    Committed, Durability, Kind, Outcome, RecordInput, SimDisk, Tape, TapeOptions as Options,
};

use crate::support;
use std::sync::atomic::Ordering;
use support::ObservedIo;

#[test]
fn short_sessions_bound_rewrite_bytes_to_previous_session_writes() {
    let io = Arc::new(ObservedIo::new(0));
    let clock = Arc::new(SimClock::default());
    let opts = Options { segment_size: 1024, ..Options::default() };
    let mut previous_writes = 0;
    for session in 0..16 {
        io.segment_bytes.store(0, Ordering::SeqCst);
        io.segment_syncs.store(0, Ordering::SeqCst);
        let (tape, _) = Tape::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
        let rewritten = io.segment_bytes.swap(0, Ordering::SeqCst);
        assert!(rewritten <= previous_writes, "session={session}, rewritten={rewritten}");
        if session > 0 {
            assert_eq!(io.segment_syncs.load(Ordering::SeqCst), 0);
        }
        // Fill a segment exactly, so preallocated-tail overhead cannot hide growing rewrites.
        let input = RecordInput { content: vec![session as u8; 912], ..record(Kind::Turn) };
        tape.append(vec![input], Durability::Durable).unwrap();
        previous_writes = io.segment_bytes.load(Ordering::SeqCst);
        assert_eq!(previous_writes, 1024);
        drop(tape);
        clock.advance(500_000_000);
    }
}

#[test]
fn short_clean_sessions_protect_previous_session_data_from_corruption() {
    use zerodb_tape::Io;
    let io = Arc::new(SimDisk::new(0));
    let clock = Arc::new(SimClock::default());
    let opts = Options { segment_size: 1024, ..Options::default() };
    for session in 1..=8 {
        let (tape, _) = Tape::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
        tape.append(vec![record(Kind::Turn)], Durability::Durable).unwrap();
        drop(tape);
        clock.advance(500_000_000);
        let image = Arc::new(io.crash().unwrap());
        let first = image.open(std::path::Path::new("db/seg-0000000000000001.zdb")).unwrap();
        image.flip_bit(first, 0).unwrap();
        image.close(first).unwrap();
        assert!(
            matches!(
                Tape::open(image, clock.clone(), "db", opts.clone()),
                Err(Error::CorruptionDetected { .. })
            ),
            "session={session}"
        );
        let dir = zerodb_tape::sim::DataDir::open(io.clone(), "db", None, 1024).unwrap();
        assert_eq!(dir.superblock(), session);
    }
}

#[test]
fn recovered_survivors_install_superblock_before_open_returns() {
    let io = Arc::new(ObservedIo::new(0));
    let clock = Arc::new(SimClock::default());
    let opts = Options { segment_size: 1024, ..Options::default() };
    let (mut core, _) = WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
    let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    reply.recv().unwrap().unwrap();
    drop(core);
    io.events.lock().unwrap().clear();
    let (core, report) = WriterCore::open(io.clone(), clock, "db", opts).unwrap();
    assert_eq!(report.durable_lsn, 1);
    drop(core);
    let events = io.events.lock().unwrap();
    let rewritten = events.iter().position(|e| e == "write db/seg-0000000000000001.zdb 0").unwrap();
    let synced = events.iter().position(|e| e == "sync db/seg-0000000000000001.zdb").unwrap();
    let installed = events.iter().position(|e| e == "rename db/SUPERBLOCK.tmp db/SUPERBLOCK");
    assert!(installed.is_some(), "recovery must install its durable boundary");
    let installed = installed.unwrap();
    let recovered_dir = events[..installed].iter().rposition(|e| e == "dir-sync db").unwrap();
    let directory = events.iter().rposition(|e| e == "dir-sync db").unwrap();
    assert!(rewritten < synced && synced < recovered_dir);
    assert!(recovered_dir < installed && installed < directory);
    let dir = zerodb_tape::sim::DataDir::open(Arc::new(io.disk.crash().unwrap()), "db", None, 1024)
        .unwrap();
    assert_eq!(dir.superblock(), 1);
}

#[test]
fn clean_shutdown_forces_superblock_after_durable_records() {
    let io = Arc::new(ObservedIo::new(0));
    let clock = Arc::new(SimClock::default());
    let opts = Options { segment_size: 1024, ..Options::default() };
    let (tape, _) = Tape::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
    tape.append(vec![record(Kind::Turn)], Durability::Durable).unwrap();
    assert_eq!(tape.durable_lsn(), 1);
    io.events.lock().unwrap().clear();
    drop(tape);
    let events = io.events.lock().unwrap().clone();
    let installed =
        events.iter().position(|e| e == "rename db/SUPERBLOCK.tmp db/SUPERBLOCK").unwrap();
    assert!(installed > 0);
    let image = Arc::new(io.disk.crash().unwrap());
    let dir = zerodb_tape::sim::DataDir::open(image.clone(), "db", None, 1024).unwrap();
    assert_eq!(dir.superblock(), 1);
    drop(dir);
    let (tape, _) = Tape::open(image, clock, "db", opts).unwrap();
    assert_eq!(&tape.read(1).unwrap().unwrap().payload, b"hello");
}

#[test]
fn failed_final_sync_does_not_advance_the_shutdown_superblock() {
    let io = Arc::new(ObservedIo::new(0));
    let (mut core, _) = WriterCore::open(
        io.clone(),
        Arc::new(SimClock::default()),
        "db",
        Options { segment_size: 1024, ..Options::default() },
    )
    .unwrap();
    let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    io.disk.fail_next_sync().unwrap();
    assert!(matches!(core.shutdown(), Err(Error::Poisoned)));
    assert_eq!(reply.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
    assert_eq!(core.durable_lsn(), 0);
    let syncs = io.syncs.load(Ordering::SeqCst);
    assert!(matches!(core.shutdown(), Err(Error::Poisoned)));
    assert_eq!(io.syncs.load(Ordering::SeqCst), syncs);
    drop(core);
    let dir = zerodb_tape::sim::DataDir::open(io, "db", None, 1024).unwrap();
    assert_eq!(dir.superblock(), 0);
}

#[test]
fn live_restart_after_failed_sync_redirties_survivors_before_acknowledging() {
    for seed in 0..200 {
        for advance_superblock in [false, true] {
            let disk = Arc::new(SimDisk::new(seed));
            let clock = Arc::new(SimClock::default());
            let opts = Options { segment_size: 1024, ..Options::default() };
            let (mut core, _) =
                WriterCore::open(disk.clone(), clock.clone(), "db", opts.clone()).unwrap();
            disk.fail_next_sync().unwrap();
            let (request, reply) = WriteRequest::new(
                PreparedBatch::new(
                    vec![RecordInput { content: b"A".to_vec(), ..record(Kind::Turn) }],
                    Durability::Durable,
                )
                .unwrap(),
            );
            core.enqueue(request);
            core.step();
            let error = reply.recv().unwrap().unwrap_err();
            assert!(matches!(error.kind, Error::SyncFailed(_)));
            assert_eq!(error.outcome, Outcome::Unknown);
            assert!(core.is_poisoned());
            drop(core);

            // Process restart only. Failed bytes remain readable through the live page cache.
            let (mut core, report) =
                WriterCore::open(disk.clone(), clock.clone(), "db", opts.clone()).unwrap();
            assert_eq!(report.durable_lsn, 1);
            let (request, reply) = WriteRequest::new(
                PreparedBatch::new(
                    vec![RecordInput { content: b"B".to_vec(), ..record(Kind::Turn) }],
                    Durability::Durable,
                )
                .unwrap(),
            );
            core.enqueue(request);
            core.step();
            assert!(matches!(
                reply.recv().unwrap().unwrap(),
                Committed::Durable { first_lsn: 2, last_lsn: 2, .. }
            ));
            if advance_superblock {
                clock.advance(30_000_000_000);
                core.update_superblock().unwrap();
            }
            drop(core);
            let (tape, report) =
                Tape::open(Arc::new(disk.crash().unwrap()), clock, "db", opts).unwrap();
            assert_eq!(report.durable_lsn, 2, "seed={seed}, superblock={advance_superblock}");
            assert_eq!(&tape.read(1).unwrap().unwrap().payload, b"A");
            assert_eq!(&tape.read(2).unwrap().unwrap().payload, b"B");
        }
    }
}

#[test]
fn live_restart_rewrites_across_rolls_and_before_a_trimmed_tail() {
    use zerodb_tape::Io;
    let io = Arc::new(ObservedIo::new(0));
    let clock = Arc::new(SimClock::default());
    let opts = Options { segment_size: 435, ..Options::default() };
    let (mut core, _) = WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
    let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    reply.recv().unwrap().unwrap();
    clock.advance(30_000_000_000);
    core.update_superblock().unwrap();
    let mut replies = Vec::new();
    for _ in 0..3 {
        let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(request);
        replies.push(reply);
    }
    // The roll performs 4 preparation syncs before the first dirty-segment sync.
    io.fail_at.store(io.syncs.load(Ordering::SeqCst) + 3, Ordering::SeqCst);
    core.step();
    for reply in replies {
        assert_eq!(reply.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
    }
    drop(core);
    let file = io.open(std::path::Path::new("db/seg-0000000000000002.zdb")).unwrap();
    io.write_at(file, 117, &[1; 8]).unwrap();
    io.close(file).unwrap();
    io.events.lock().unwrap().clear();
    let (mut core, report) =
        WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
    assert_eq!(report.durable_lsn, 4);
    assert!(report.truncated.is_some());
    let events = io.events.lock().unwrap().clone();
    assert!(events.iter().any(|e| e == "write db/seg-0000000000000001.zdb 117"));
    assert!(events.iter().any(|e| e == "write db/seg-0000000000000002.zdb 0"));
    assert!(!events.iter().any(|e| e == "write db/seg-0000000000000001.zdb 0"));
    let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    reply.recv().unwrap().unwrap();
    drop(core);
    let (tape, report) = Tape::open(Arc::new(io.disk.crash().unwrap()), clock, "db", opts).unwrap();
    assert_eq!(report.durable_lsn, 5);
    for lsn in 1..=5 {
        assert_eq!(&tape.read(lsn).unwrap().unwrap().payload, b"hello");
    }
}

#[test]
fn live_retry_of_failed_tail_repair_persists_roll_padding() {
    use zerodb_tape::Io;
    for seed in 0..32 {
        for advance_superblock in [false, true] {
            let io = Arc::new(ObservedIo::new(seed));
            let clock = Arc::new(SimClock::default());
            let opts = Options { segment_size: 1024, ..Options::default() };
            let (mut core, _) =
                WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
            let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
            core.enqueue(request);
            core.step();
            reply.recv().unwrap().unwrap();
            clock.advance(30_000_000_000);
            core.update_superblock().unwrap();
            drop(core);
            let file = io.open(std::path::Path::new("db/seg-0000000000000001.zdb")).unwrap();
            io.disk.write_at(file, 900, &[1; 100]).unwrap();
            io.disk.sync_data(file).unwrap();
            io.close(file).unwrap();
            io.fail_at.store(io.syncs.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
            assert!(WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).is_err());

            // Failed zero-tail writeback leaves clean cached zeros over durable garbage.
            let (mut core, report) =
                WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
            assert_eq!(report.durable_lsn, 1);
            for _ in 0..3 {
                let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
                core.enqueue(request);
                core.step();
                reply.recv().unwrap().unwrap();
            }
            if advance_superblock {
                clock.advance(30_000_000_000);
                core.update_superblock().unwrap();
            }
            drop(core);
            let (tape, report) =
                Tape::open(Arc::new(io.disk.crash().unwrap()), clock, "db", opts).unwrap();
            assert_eq!(report.durable_lsn, 4, "seed={seed}, superblock={advance_superblock}");
            for lsn in 1..=4 {
                assert!(tape.read(lsn).unwrap().is_some());
            }
        }
    }
}

#[test]
fn concurrent_callers_get_dense_lsn_and_immediate_visibility() {
    let disk = Arc::new(SimDisk::new(0));
    let (tape, _) = Tape::open(disk, Arc::new(SimClock::new(0)), "db", Options::default()).unwrap();
    let tape = Arc::new(tape);
    let gate = Arc::new(Barrier::new(8));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let tape = tape.clone();
        let gate = gate.clone();
        threads.push(std::thread::spawn(move || {
            gate.wait();
            let mut lsns = Vec::new();
            for _ in 0..20 {
                let Committed::Durable { txn, first_lsn, last_lsn, .. } =
                    tape.append(vec![record(Kind::Turn)], Durability::Durable).unwrap();
                assert_eq!(first_lsn, last_lsn);
                assert_eq!(txn, first_lsn);
                assert_eq!(tape.read(last_lsn).unwrap().unwrap().payload, *b"hello");
                lsns.push(last_lsn);
            }
            lsns
        }));
    }
    let mut lsns: Vec<_> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
    lsns.sort();
    assert_eq!(lsns, (1..=160).collect::<Vec<_>>());
}

#[test]
fn sync_failure_is_unknown_then_poisoned_reads_keep_working() {
    let disk = Arc::new(SimDisk::new(0));
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::new(0)), "db", Options::default())
            .unwrap();
    for lsn in 1..=2 {
        let (request, committed) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(request);
        core.step();
        committed.recv().unwrap().unwrap();
        assert_eq!(core.durable_lsn(), lsn, "acknowledged lsn={lsn}");
        for visible in 1..=lsn {
            assert!(core.read(visible).unwrap().is_some(), "ack={lsn}, visible={visible}");
        }
    }
    disk.fail_next_sync().unwrap();
    let (request, failed) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    assert_eq!(failed.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
    assert!(core.read(1).unwrap().is_some());
    assert!(core.read(2).unwrap().is_some());
    assert!(core.read(3).unwrap().is_none());
    let (request, later) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    let error = later.recv().unwrap().unwrap_err();
    assert!(matches!(error.kind, Error::Poisoned));
    assert_eq!(error.outcome, Outcome::NotCommitted);
}

#[test]
fn invalid_batches_never_enter_the_writer() {
    for (case, batch, oversized) in [
        ("too many records", vec![record(Kind::Turn); 1025], true),
        (
            "frame exceeds byte limit",
            vec![RecordInput { content: vec![0; 4 * 1024 * 1024], ..record(Kind::Turn) }],
            true,
        ),
        ("caller supplied footer", vec![record(Kind::Footer)], false),
        ("empty batch", vec![], false),
    ] {
        let error = PreparedBatch::new(batch, Durability::Durable).unwrap_err();
        assert_eq!(error.outcome, Outcome::NotCommitted, "{case}");
        if oversized {
            assert!(matches!(error.kind, Error::BatchTooLarge), "{case}");
        }
    }
}

#[test]
fn superblock_installation_obeys_clock_byte_and_roll_thresholds() {
    use zerodb_tape::sim::DataDir;

    for (case, segment_size, budget, count) in [
        ("clock threshold", 4096, 256 * 1024 * 1024, 1),
        ("byte threshold", 4096, 300, 3),
        ("segment roll", 200, 256 * 1024 * 1024, 2),
    ] {
        let io = Arc::new(ObservedIo::new(35));
        let clock = Arc::new(SimClock::default());
        let opts = Options { segment_size, superblock_bytes: budget, ..Options::default() };
        let (mut core, _) = WriterCore::open(io.clone(), clock.clone(), "db", opts).unwrap();
        io.events.lock().unwrap().clear();
        let boundary = || {
            DataDir::open(Arc::new(io.disk.crash().unwrap()), "db", None, segment_size)
                .unwrap()
                .superblock()
        };
        for lsn in 1..=count {
            let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
            core.enqueue(request);
            core.step();
            reply.recv().unwrap().unwrap();
            core.update_superblock().unwrap();
            let expected = if case != "clock threshold" && lsn == count { count } else { 0 };
            assert_eq!(boundary(), expected, "{case}, lsn={lsn}");
        }
        if case == "clock threshold" {
            clock.advance(29_999_999_999);
            core.update_superblock().unwrap();
            assert_eq!(boundary(), 0, "{case}, before threshold");
            clock.advance(1);
            core.update_superblock().unwrap();
            assert_eq!(boundary(), 1, "{case}, at threshold");
        }
        let installs = io
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event.as_str() == "rename db/SUPERBLOCK.tmp db/SUPERBLOCK")
            .count();
        assert_eq!(installs, 1, "{case}");
    }
}

#[test]
fn batch_larger_than_configured_segment_does_not_poison() {
    let (mut core, _) = WriterCore::open(
        Arc::new(SimDisk::new(0)),
        Arc::new(SimClock::new(0)),
        "db",
        Options { segment_size: 512, ..Options::default() },
    )
    .unwrap();
    let batch = PreparedBatch::new(
        vec![RecordInput { content: vec![0; 1000], ..record(Kind::Turn) }],
        Durability::Durable,
    )
    .unwrap();
    let (request, reply) = WriteRequest::new(batch);
    core.enqueue(request);
    core.step();
    assert!(matches!(
        reply.recv().unwrap().unwrap_err(),
        zerodb_tape::WriteError { kind: Error::BatchTooLarge, outcome: Outcome::NotCommitted }
    ));
    assert!(!core.is_poisoned());
    let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    reply.recv().unwrap().unwrap();
}

#[test]
fn one_step_group_shares_one_sync_and_closes_all_handles() {
    let io = Arc::new(ObservedIo::new(0));
    let (mut core, _) = WriterCore::open(
        io.clone(),
        Arc::new(SimClock::new(0)),
        "db",
        Options { segment_size: 65536, ..Options::default() },
    )
    .unwrap();
    io.syncs.store(0, Ordering::SeqCst);
    let mut replies = Vec::new();
    for _ in 0..20 {
        let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(request);
        replies.push(reply);
    }
    assert_eq!(core.durable_lsn(), 0);
    core.step();
    assert_eq!(io.syncs.load(Ordering::SeqCst), 1);
    for reply in replies {
        reply.recv().unwrap().unwrap();
    }
    drop(core);
    assert!(io.handles.lock().unwrap().is_empty());
}

#[test]
fn slow_sync_groups_concurrent_callers() {
    let io = Arc::new(ObservedIo::new(0));
    let (tape, _) = Tape::open(
        io.clone(),
        Arc::new(SimClock::new(0)),
        "db",
        Options { segment_size: 65536, ..Options::default() },
    )
    .unwrap();
    let tape = Arc::new(tape);
    io.syncs.store(0, Ordering::SeqCst);
    io.slow_ms.store(50, Ordering::SeqCst);
    let gate = Arc::new(Barrier::new(16));
    let mut threads = Vec::new();
    for _ in 0..16 {
        let tape = tape.clone();
        let gate = gate.clone();
        threads.push(std::thread::spawn(move || {
            gate.wait();
            tape.append(vec![record(Kind::Turn)], Durability::Durable).unwrap();
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(io.syncs.load(Ordering::SeqCst) < 16);
}

#[test]
fn batch_is_all_or_nothing_on_partial_write() {
    let io = Arc::new(SimDisk::new(0));
    let opts = Options { segment_size: 65536, ..Options::default() };
    let (mut core, _) =
        WriterCore::open(io.clone(), Arc::new(SimClock::new(0)), "db", opts.clone()).unwrap();
    io.enospc_after_bytes(200).unwrap();
    let batch = PreparedBatch::new(vec![record(Kind::Turn); 3], Durability::Durable).unwrap();
    let (request, reply) = WriteRequest::new(batch);
    core.enqueue(request);
    core.step();
    let error = reply.recv().unwrap().unwrap_err();
    assert_eq!(error.outcome, Outcome::NotCommitted);
    assert!(matches!(error.kind, Error::DiskFull));
    assert!(core.is_poisoned());
    assert!(core.read(1).unwrap().is_none());
    drop(core);
    let crash = Arc::new(io.crash().unwrap());
    let (core, report) = WriterCore::open(crash, Arc::new(SimClock::new(0)), "db", opts).unwrap();
    assert_eq!(report.records, 0);
    assert_eq!(core.durable_lsn(), 0);
}

#[test]
fn periodic_superblock_is_off_reply_path_and_rate_limited() {
    let io = Arc::new(ObservedIo::new(0));
    let clock = Arc::new(SimClock::new(0));
    let (mut core, _) = WriterCore::open(
        io.clone(),
        clock.clone(),
        "db",
        Options { segment_size: 65536, ..Options::default() },
    )
    .unwrap();
    io.syncs.store(0, Ordering::SeqCst);
    for i in 0..3 {
        let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(request);
        core.step();
        reply.try_recv().unwrap().unwrap();
        if i == 1 {
            clock.advance(30_000_000_000);
        }
        core.update_superblock().unwrap();
    }
    assert_eq!(io.syncs.load(Ordering::SeqCst), 4);
    drop(core);
    let dir = zerodb_tape::sim::DataDir::open(io.clone(), "db", None, 65536).unwrap();
    assert_eq!(dir.superblock(), 2);
    drop(dir);
    assert!(io.handles.lock().unwrap().is_empty());
}

#[test]
fn crash_during_a_group_spanning_roll_retains_every_prior_ack() {
    for seed in 0..100 {
        let io = Arc::new(ObservedIo::new(seed));
        let opts = Options { segment_size: 512, ..Options::default() };
        let (mut core, _) =
            WriterCore::open(io.clone(), Arc::new(SimClock::new(0)), "db", opts.clone()).unwrap();
        let (request, first) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(request);
        core.step();
        first.recv().unwrap().unwrap();
        io.capture.store(true, Ordering::SeqCst);
        let mut replies = Vec::new();
        for _ in 0..3 {
            let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
            core.enqueue(request);
            replies.push(reply);
        }
        core.step();
        for reply in replies {
            reply.recv().unwrap().unwrap();
        }
        for image in io.images.lock().unwrap().iter() {
            let (reopened, _) = WriterCore::open(
                Arc::new(image.clone()),
                Arc::new(SimClock::new(0)),
                "db",
                opts.clone(),
            )
            .unwrap();
            assert_eq!(&reopened.read(1).unwrap().unwrap().payload, b"hello", "seed={seed}");
        }
        let (mut reopened, report) = WriterCore::open(
            Arc::new(io.disk.crash().unwrap()),
            Arc::new(SimClock::new(0)),
            "db",
            opts.clone(),
        )
        .unwrap();
        assert_eq!(report.records, 4, "seed={seed}");
        for lsn in 1..=4 {
            assert_eq!(&reopened.read(lsn).unwrap().unwrap().payload, b"hello");
        }
        let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
        reopened.enqueue(request);
        reopened.step();
        assert!(matches!(reply.recv().unwrap().unwrap(), Committed::Durable { first_lsn: 5, .. }));
    }
}

#[test]
fn failed_sync_after_roll_keeps_entire_group_unpublished() {
    let io = Arc::new(ObservedIo::new(0));
    let (mut core, _) = WriterCore::open(
        io.clone(),
        Arc::new(SimClock::new(0)),
        "db",
        Options { segment_size: 128, ..Options::default() },
    )
    .unwrap();
    io.syncs.store(0, Ordering::SeqCst);
    io.fail_at.store(6, Ordering::SeqCst);
    let mut replies = Vec::new();
    for _ in 0..3 {
        let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(request);
        replies.push(reply);
    }
    core.step();
    for reply in replies {
        assert_eq!(reply.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
    }
    assert_eq!(core.durable_lsn(), 0);
    assert!(core.is_poisoned());
    for lsn in 1..=3 {
        assert!(core.read(lsn).unwrap().is_none());
    }
    drop(core);
    assert!(io.handles.lock().unwrap().is_empty());
}

#[test]
fn successful_group_prefix_commits_when_the_next_write_is_partial() {
    let io = Arc::new(SimDisk::new(0));
    let opts = Options { segment_size: 65536, ..Options::default() };
    let (mut core, _) =
        WriterCore::open(io.clone(), Arc::new(SimClock::new(0)), "db", opts.clone()).unwrap();
    io.enospc_after_bytes(117 + 100).unwrap();
    let (first, first_reply) = WriteRequest::new(prepared(Durability::Durable));
    let (second, second_reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(first);
    core.enqueue(second);
    core.step();
    assert!(matches!(first_reply.recv().unwrap().unwrap(), Committed::Durable { last_lsn: 1, .. }));
    assert_eq!(second_reply.recv().unwrap().unwrap_err().outcome, Outcome::NotCommitted);
    let (reopened, report) =
        WriterCore::open(Arc::new(io.crash().unwrap()), Arc::new(SimClock::new(0)), "db", opts)
            .unwrap();
    assert_eq!(report.records, 1);
    assert!(reopened.read(1).unwrap().is_some());
    assert!(reopened.read(2).unwrap().is_none());
}

#[test]
fn metadata_sync_failure_cannot_retract_an_acknowledgement() {
    let io = Arc::new(SimDisk::new(0));
    let clock = Arc::new(SimClock::new(0));
    let (mut core, _) = WriterCore::open(
        io.clone(),
        clock.clone(),
        "db",
        Options { segment_size: 65536, ..Options::default() },
    )
    .unwrap();
    let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    reply.try_recv().unwrap().unwrap();
    clock.advance(30_000_000_000);
    io.fail_next_sync().unwrap();
    assert!(core.update_superblock().is_err());
    assert!(core.is_poisoned());
    assert!(core.read(1).unwrap().is_some());
}

#[test]
fn prefix_commits_when_tail_reservation_fails_unless_sparse_resize_also_fails() {
    use std::path::Path;
    use zerodb_tape::Io;

    for fallback_fails in [false, true] {
        let io = Arc::new(ObservedIo::new(19));
        let opts = Options { segment_size: 4096, free_space_floor: Some(0), ..Options::default() };
        let clock = Arc::new(SimClock::default());
        let (mut core, _) =
            WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
        let used = io.disk.total_bytes(Path::new("db")).unwrap()
            - io.disk.available_bytes(Path::new("db")).unwrap();
        *io.capacity_after_space_check.lock().unwrap() = Some(used - 512);
        io.disk.enospc_after_bytes(117 + 100).unwrap();
        io.fail_extension.store(fallback_fails, Ordering::SeqCst);
        let (first, first_reply) = WriteRequest::new(prepared(Durability::Durable));
        let (second, second_reply) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(first);
        core.enqueue(second);
        core.step();
        let first = first_reply.recv().unwrap();
        let second = second_reply.recv().unwrap().unwrap_err();
        assert!(core.is_poisoned());
        assert!(matches!(second.kind, Error::DiskFull));
        if fallback_fails {
            assert_eq!(first.unwrap_err().outcome, Outcome::Unknown);
            assert_eq!(second.outcome, Outcome::Unknown);
            assert_eq!(core.durable_lsn(), 0);
            continue;
        }
        assert!(matches!(first.unwrap(), Committed::Durable { last_lsn: 1, .. }));
        assert_eq!(second.outcome, Outcome::NotCommitted);
        assert_eq!(core.durable_lsn(), 1);
        assert_eq!(io.disk.available_bytes(Path::new("db")).unwrap(), 3072);
        let image = Arc::new(io.disk.crash().unwrap());
        image.set_capacity(1 << 30).unwrap();
        drop(core);
        let (reopened, report) = WriterCore::open(image, clock, "db", opts).unwrap();
        assert_eq!(report.records, 1);
        assert_eq!(reopened.read(1).unwrap().unwrap().payload, b"hello");
        assert!(reopened.read(2).unwrap().is_none());
    }
}

#[test]
fn real_files_reopen_with_multiple_tenants_and_rebuilt_chain_heads() {
    let path =
        std::env::current_dir().unwrap().join(format!("target/tape-test-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let io = Arc::new(zerodb_tape::StdIo::new());
    let clock = Arc::new(SimClock::new(0));
    let opts = Options { segment_size: 65536, ..Options::default() };
    {
        let (tape, _) = Tape::open(io.clone(), clock.clone(), &path, opts.clone()).unwrap();
        tape.append(vec![record(Kind::Turn)], Durability::Durable).unwrap();
        let committed = tape
            .append(
                vec![
                    record(Kind::Turn),
                    RecordInput { tenant_id: 2, ..record(Kind::Fact) },
                    record(Kind::Forget),
                ],
                Durability::Durable,
            )
            .unwrap();
        assert!(matches!(committed, Committed::Durable { first_lsn: 2, last_lsn: 4, .. }));
    }
    {
        let (tape, report) = Tape::open(io.clone(), clock.clone(), &path, opts.clone()).unwrap();
        assert_eq!(report.records, 4);
        for lsn in 1..=4 {
            assert_eq!(&tape.read(lsn).unwrap().unwrap().payload, b"hello");
        }
        tape.append(vec![record(Kind::Turn)], Durability::Durable).unwrap();
    }
    let (tape, report) = Tape::open(io, clock, &path, opts).unwrap();
    assert_eq!(report.records, 5);
    drop(tape);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn segment_size_mismatch_is_rejected_without_mutation() {
    use std::path::Path;
    use zerodb_tape::Io;
    for wrong_size in [512, 2048] {
        let io = Arc::new(ObservedIo::new(0));
        let clock = Arc::new(SimClock::new(0));
        let opts = Options { segment_size: 1024, ..Options::default() };
        let (mut core, _) =
            WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
        let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
        core.enqueue(request);
        core.step();
        reply.recv().unwrap().unwrap();
        drop(core);
        let snapshot = || {
            io.list(Path::new("db"))
                .unwrap()
                .into_iter()
                .map(|path| {
                    let file = io.open(&path).unwrap();
                    let mut bytes = vec![0; io.len(file).unwrap() as usize];
                    io.read_at(file, 0, &mut bytes).unwrap();
                    io.close(file).unwrap();
                    (path, bytes)
                })
                .collect::<Vec<_>>()
        };
        let before = snapshot();
        io.events.lock().unwrap().clear();
        let result = WriterCore::open(
            io.clone(),
            clock.clone(),
            "db",
            Options { segment_size: wrong_size, ..opts.clone() },
        );
        assert!(matches!(result, Err(Error::Configuration(_))), "size={wrong_size}");
        assert!(io.events.lock().unwrap().is_empty(), "mismatch must not write or sync");
        assert_eq!(snapshot(), before);
        assert!(io.handles.lock().unwrap().is_empty());
        let (_, report) = WriterCore::open(io, clock, "db", opts).unwrap();
        assert_eq!(report.records, 1);
    }
}

#[test]
fn commit_time_survives_a_restart_with_a_backwards_clock() {
    use std::sync::Arc;
    use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
    use zerodb_tape::{Durability, RecordInput, SimDisk, TapeOptions};
    let disk = Arc::new(SimDisk::new(1));
    let opts = TapeOptions { segment_size: 1024, ..TapeOptions::default() };
    let mut previous = 0;
    for clock in [1000, 1, 0] {
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::new(clock)), "db", opts.clone())
                .unwrap();
        let batch = PreparedBatch::new(
            vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![] }],
            Durability::Durable,
        )
        .unwrap();
        let (request, reply) = WriteRequest::new(batch);
        core.enqueue(request);
        core.step();
        reply.recv().unwrap().unwrap();
        let time = core.read(core.durable_lsn()).unwrap().unwrap().header.commit_time_ns;
        assert!(time > previous);
        previous = time;
    }
}

mod public_api {
    use std::sync::Arc;
    use zerodb_tape::{
        Clock, Committed, Digest, Durability, FileId, Frame, Header, Io, Kind, LockGuard, Outcome,
        RecordInput, RecoveryReport, SimDisk, StdIo, SystemClock, Tape, TapeOptions, WriteError,
        WriteResult,
    };

    #[test]
    fn root_api_supports_append_read_and_custom_backends() {
        let disk = Arc::new(SimDisk::new(0));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let io: Arc<dyn Io> = disk;
        let _: StdIo = StdIo::new();
        let _: FileId = FileId(1);
        let _: Digest = [0; 32];
        let _: Option<Header> = None;
        let _: Option<Frame> = None;
        let _: Option<RecoveryReport> = None;
        let _: WriteResult =
            Err(WriteError { kind: zerodb_tape::Error::Poisoned, outcome: Outcome::NotCommitted });
        let _guard = LockGuard::new(|| {});
        let (tape, _) = Tape::open(io, clock, "db", TapeOptions::default()).unwrap();
        let record = RecordInput { tenant_id: 1, kind: Kind::Turn, content: b"public".to_vec() };
        let Committed::Durable { first_lsn, .. } =
            tape.append(vec![record], Durability::Durable).unwrap();
        assert_eq!(&tape.read(first_lsn).unwrap().unwrap().payload, b"public");
    }
}

mod tenants {
    use std::sync::Arc;
    use zerodb_tape::sim::SimClock;
    use zerodb_tape::{Durability, Kind, RecordInput, SimDisk, Tape, TapeOptions};

    #[test]
    fn tenant_create_is_decodable_unchained_and_atomic_with_turns() {
        let disk = Arc::new(SimDisk::new(5));
        let opts = TapeOptions { segment_size: 4096, ..TapeOptions::default() };
        let (tape, _) =
            Tape::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone()).unwrap();
        for _ in 0..2 {
            tape.append(
                vec![
                    RecordInput {
                        kind: Kind::TenantCreate,
                        tenant_id: 1,
                        content: b"tenant".to_vec(),
                    },
                    RecordInput { kind: Kind::Turn, tenant_id: 1, content: b"turn".to_vec() },
                ],
                Durability::Durable,
            )
            .unwrap();
        }
        let tenant = tape.read(3).unwrap().unwrap();
        assert_eq!(tenant.header.kind, Kind::TenantCreate);
        drop(tape);
        let (tape, _) = Tape::open(disk, Arc::new(SimClock::default()), "db", opts).unwrap();
        assert_eq!(tape.scan(0).count(), 4);
    }
}

mod concurrency {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};
    use zerodb_tape::{
        Committed, Durability, Kind, RecordInput, StdIo, SystemClock, Tape, TapeOptions as Options,
    };

    #[test]
    #[ignore = "20,000 filesystem syncs; run the slow-tests CI job"]
    fn concurrent_durable_appends_survive_reopen() {
        let path =
            std::env::current_dir().unwrap().join(format!("target/stress-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let io = Arc::new(StdIo::new());
        let clock = Arc::new(SystemClock);
        let (tape, _) = Tape::open(io.clone(), clock.clone(), &path, Options::default()).unwrap();
        let barrier = Barrier::new(16);
        let acknowledged = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..16u32)
                .map(|tenant| {
                    let tape = &tape;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let mut rng = u64::from(tenant) + 1;
                        barrier.wait();
                        (0..1250u32)
                            .map(|sequence| {
                                rng ^= rng << 13;
                                rng ^= rng >> 7;
                                rng ^= rng << 17;
                                let mut content = vec![rng as u8; (rng % 8193) as usize];
                                let prefix = sequence.to_le_bytes();
                                let count = content.len().min(prefix.len());
                                content[..count].copy_from_slice(&prefix[..count]);
                                let record = RecordInput {
                                    tenant_id: tenant,
                                    kind: Kind::Turn,
                                    content: content.clone(),
                                };
                                let ack = tape.append(vec![record], Durability::Durable).unwrap();
                                let Committed::Durable { first_lsn, last_lsn, .. } = ack;
                                assert_eq!(first_lsn, last_lsn);
                                (first_lsn, tenant, content)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            threads.into_iter().flat_map(|thread| thread.join().unwrap()).collect::<Vec<_>>()
        });
        assert_eq!(acknowledged.len(), 20_000);
        assert_eq!(tape.durable_lsn(), 20_000);
        drop(tape);
        let (tape, report) = Tape::open(io, clock, &path, Options::default()).unwrap();
        assert_eq!(report.records, 20_000);
        let mut unique = BTreeMap::new();
        for (lsn, tenant, content) in acknowledged {
            assert!(unique.insert(lsn, ()).is_none());
            let frame = tape.read(lsn).unwrap().unwrap();
            assert_eq!(frame.header.lsn, lsn);
            assert_eq!(frame.header.tenant_id, tenant);
            assert_eq!(frame.header.kind, Kind::Turn);
            assert_eq!(frame.payload, content);
        }
        drop(tape);
        std::fs::remove_dir_all(path).unwrap();
    }
}
