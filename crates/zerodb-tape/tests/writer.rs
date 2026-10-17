#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Barrier};
use zerodb_tape::frame::Kind;
use zerodb_tape::io::SimClock;
use zerodb_tape::simdisk::SimDisk;
use zerodb_tape::writer::{
    Committed, Durability, Options, Outcome, PreparedBatch, RecordInput, Tape, WriteRequest,
    WriterCore,
};
use zerodb_tape::Error;

mod support;
use std::sync::atomic::Ordering;
use support::ObservedIo;

fn record(kind: Kind) -> RecordInput {
    RecordInput { kind, tenant_id: 1, content: b"hello".to_vec() }
}

fn prepared(mode: Durability) -> PreparedBatch {
    PreparedBatch::with_salts(
        vec![record(if mode == Durability::Fast { Kind::Cache } else { Kind::Turn })],
        mode,
        || Ok([7; 16]),
    )
    .unwrap()
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
                let Committed::Durable { txn, first_lsn, last_lsn } =
                    tape.append(vec![record(Kind::Turn)], Durability::Durable).unwrap()
                else {
                    panic!()
                };
                assert_eq!(first_lsn, last_lsn);
                assert_eq!(txn, first_lsn);
                assert_eq!(tape.read(last_lsn).unwrap().unwrap().payload[16..], *b"hello");
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
    let (request, committed) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    committed.recv().unwrap().unwrap();
    disk.fail_next_sync().unwrap();
    let (request, failed) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    assert_eq!(failed.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
    assert!(core.read(1).unwrap().is_some());
    assert!(core.read(2).unwrap().is_none());
    let (request, later) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    let error = later.recv().unwrap().unwrap_err();
    assert!(matches!(error.kind, Error::Poisoned));
    assert_eq!(error.outcome, Outcome::NotCommitted);
}

#[test]
fn fast_has_no_identifier_and_is_invisible_until_next_sync() {
    let disk = Arc::new(SimDisk::new(0));
    let (mut core, _) =
        WriterCore::open(disk, Arc::new(SimClock::new(0)), "db", Options::default()).unwrap();
    let (request, reply) = WriteRequest::new(prepared(Durability::Fast));
    core.enqueue(request);
    core.step();
    assert_eq!(reply.recv().unwrap().unwrap(), Committed::Fast);
    assert!(core.read(1).unwrap().is_none());
    let (request, reply) = WriteRequest::new(prepared(Durability::Durable));
    core.enqueue(request);
    core.step();
    reply.recv().unwrap().unwrap();
    assert!(core.read(1).unwrap().is_some());
}

#[test]
fn invalid_batches_never_enter_the_writer() {
    for (batch, mode) in [
        (vec![record(Kind::Turn); 1025], Durability::Durable),
        (
            vec![RecordInput { content: vec![0; 4 * 1024 * 1024], ..record(Kind::Turn) }],
            Durability::Durable,
        ),
        (vec![record(Kind::Turn)], Durability::Fast),
        (vec![record(Kind::Footer)], Durability::Durable),
        (vec![], Durability::Durable),
    ] {
        let error = PreparedBatch::with_salts(batch, mode, || Ok([0; 16])).unwrap_err();
        assert_eq!(error.outcome, Outcome::NotCommitted);
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
    let batch = PreparedBatch::with_salts(
        vec![RecordInput { content: vec![0; 1000], ..record(Kind::Turn) }],
        Durability::Durable,
        || Ok([0; 16]),
    )
    .unwrap();
    let (request, reply) = WriteRequest::new(batch);
    core.enqueue(request);
    core.step();
    assert!(matches!(
        reply.recv().unwrap().unwrap_err(),
        zerodb_tape::writer::WriteError {
            kind: Error::BatchTooLarge,
            outcome: Outcome::NotCommitted
        }
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
    let batch =
        PreparedBatch::with_salts(vec![record(Kind::Turn); 3], Durability::Durable, || Ok([0; 16]))
            .unwrap();
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
            clock.advance(1_000_000_000);
        }
        core.update_superblock().unwrap();
    }
    assert_eq!(io.syncs.load(Ordering::SeqCst), 4);
    drop(core);
    let dir = zerodb_tape::dir::DataDir::open(io.clone(), "db", None, 65536).unwrap();
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
            assert_eq!(&reopened.read(1).unwrap().unwrap().payload[16..], b"hello", "seed={seed}");
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
            assert_eq!(&reopened.read(lsn).unwrap().unwrap().payload[16..], b"hello");
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
        Options { segment_size: 512, ..Options::default() },
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
    io.enospc_after_bytes(273 + 100).unwrap();
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
    clock.advance(1_000_000_000);
    io.fail_next_sync().unwrap();
    assert!(core.update_superblock().is_err());
    assert!(core.is_poisoned());
    assert!(core.read(1).unwrap().is_some());
}

#[test]
fn real_files_reopen_with_multiple_tenants_and_rebuilt_chain_heads() {
    let path =
        std::env::current_dir().unwrap().join(format!("target/tape-test-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let io = Arc::new(zerodb_tape::io::StdIo::new());
    let clock = Arc::new(SimClock::new(0));
    let opts = Options { segment_size: 65536, ..Options::default() };
    {
        let (tape, _) = Tape::open(io.clone(), clock.clone(), &path, opts.clone()).unwrap();
        assert_eq!(
            tape.append(vec![record(Kind::Cache)], Durability::Fast).unwrap(),
            Committed::Fast
        );
        let committed = tape
            .append(
                vec![
                    record(Kind::Turn),
                    RecordInput { tenant_id: 2, ..record(Kind::Fact) },
                    record(Kind::Redaction),
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
            assert_eq!(&tape.read(lsn).unwrap().unwrap().payload[16..], b"hello");
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
    use zerodb_tape::io::Io;
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
