#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Barrier, Mutex};
use zerodb_tape::sim::{SimClock, WriteRequest, WriterCore};
use zerodb_tape::{
    Committed, CommittedRecord, Durability, Error, Kind, Outcome, Prepare, PreparedBatch,
    RecordInput, Result, SimDisk, StateMachine, Tape, TapeOptions, WriteError,
};

#[derive(Debug, Default)]
struct Log {
    published: Vec<CommittedRecord>,
    replayed: Vec<CommittedRecord>,
    prepares: usize,
    threads: Vec<std::thread::ThreadId>,
}

#[derive(Debug, Default)]
struct Machine {
    log: Arc<Mutex<Log>>,
    seq: u64,
    fail_replay: bool,
}

fn cached() -> Committed {
    Committed::Durable { txn: 77, first_lsn: 88, last_lsn: 88 }
}

impl StateMachine for Machine {
    fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare {
        let mut log = self.log.lock().unwrap();
        log.prepares += 1;
        log.threads.push(std::thread::current().id());
        match batch.records().next().unwrap().content.as_slice() {
            b"reject" => {
                return Prepare::Reject(WriteError {
                    kind: Error::InvalidFrame("rejected".into()),
                    outcome: Outcome::NotCommitted,
                });
            }
            b"reply" => return Prepare::Reply(cached()),
            _ => {}
        }
        for record in batch.records_mut() {
            self.seq += 1;
            record.content = self.seq.to_le_bytes().to_vec();
        }
        Prepare::Write
    }

    fn publish(&mut self, committed: &[CommittedRecord]) {
        self.log.lock().unwrap().published.extend_from_slice(committed);
    }

    fn replay(&mut self, record: &CommittedRecord) -> Result<()> {
        if self.fail_replay {
            return Err(Error::Configuration("replay failed".into()));
        }
        self.seq = u64::from_le_bytes(record.payload.as_ref().try_into().unwrap());
        self.log.lock().unwrap().replayed.push(record.clone());
        Ok(())
    }
}

fn input(kind: Kind, content: &[u8]) -> RecordInput {
    RecordInput { kind, tenant_id: 7, content: content.to_vec() }
}

fn options() -> TapeOptions {
    TapeOptions { segment_size: 512, ..TapeOptions::default() }
}

fn enqueue<S: StateMachine>(
    core: &mut WriterCore<S>,
    kind: Kind,
    mode: Durability,
) -> std::sync::mpsc::Receiver<zerodb_tape::WriteResult> {
    let batch =
        PreparedBatch::with_salts(vec![input(kind, &[0; 8])], mode, || Ok([3; 16])).unwrap();
    let (request, reply) = WriteRequest::new(batch);
    core.enqueue(request);
    reply
}

#[test]
fn writer_assigns_dense_sequences_and_publishes_before_reply_under_eight_threads() {
    let machine = Machine::default();
    let log = machine.log.clone();
    let (tape, _) = Tape::open_with(
        machine,
        Arc::new(SimDisk::new(1)),
        Arc::new(SimClock::default()),
        "db",
        options(),
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
                        .append(vec![input(Kind::Turn, &[0; 8]); 2], Durability::Durable)
                        .unwrap()
                    else {
                        panic!("expected durable reply");
                    };
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
    let machine = Machine::default();
    let log = machine.log.clone();
    let (tape, _) = Tape::open_with(
        machine,
        Arc::new(SimDisk::new(2)),
        Arc::new(SimClock::default()),
        "db",
        options(),
    )
    .unwrap();
    let error = tape.append(vec![input(Kind::Turn, b"reject")], Durability::Durable).unwrap_err();
    assert_eq!(error.outcome, Outcome::NotCommitted);
    assert_eq!(
        tape.append(vec![input(Kind::Turn, b"reply")], Durability::Durable).unwrap(),
        cached()
    );
    assert_eq!(tape.durable_lsn(), 0);
    assert_eq!(tape.scan(0).count(), 0);
    assert!(log.lock().unwrap().published.is_empty());
    assert!(matches!(
        tape.append(vec![input(Kind::Turn, &[0; 8])], Durability::Durable).unwrap(),
        Committed::Durable { txn: 1, first_lsn: 1, last_lsn: 1 }
    ));
}

#[test]
fn fast_records_wait_for_later_sync_and_scan_has_a_fixed_durable_boundary() {
    let machine = Machine::default();
    let log = machine.log.clone();
    let (mut core, _) = WriterCore::open_with(
        machine,
        Arc::new(SimDisk::new(3)),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { segment_size: 2048, ..options() },
    )
    .unwrap();
    for _ in 0..3 {
        let reply = enqueue(&mut core, Kind::Cache, Durability::Fast);
        core.step();
        assert_eq!(reply.recv().unwrap().unwrap(), Committed::Fast);
    }
    assert!(log.lock().unwrap().published.is_empty());
    assert_eq!(core.scan(0).count(), 0);
    let reply = enqueue(&mut core, Kind::Turn, Durability::Durable);
    core.step();
    reply.recv().unwrap().unwrap();
    assert_eq!(
        log.lock().unwrap().published.iter().map(|r| r.lsn).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    let old_scan = core.scan(0).collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(old_scan.len(), 4);
    let reply = enqueue(&mut core, Kind::Cache, Durability::Fast);
    core.step();
    reply.recv().unwrap().unwrap();
    assert_eq!(core.scan(0).count(), 4);
    core.shutdown().unwrap();
    assert_eq!(log.lock().unwrap().published.len(), 5);
    core.shutdown().unwrap();
    assert_eq!(log.lock().unwrap().published.len(), 5);
}

#[test]
fn failed_sync_never_publishes_and_poison_hides_prepare_side_effects() {
    for seed in 0..64 {
        let disk = Arc::new(SimDisk::new(seed));
        let machine = Machine::default();
        let log = machine.log.clone();
        let (mut core, _) = WriterCore::open_with(
            machine,
            disk.clone(),
            Arc::new(SimClock::default()),
            "db",
            options(),
        )
        .unwrap();
        let fast = enqueue(&mut core, Kind::Cache, Durability::Fast);
        core.step();
        assert_eq!(fast.recv().unwrap().unwrap(), Committed::Fast);
        let reply = enqueue(&mut core, Kind::Turn, Durability::Durable);
        disk.fail_next_sync().unwrap();
        core.step();
        assert!(matches!(reply.recv().unwrap().unwrap_err().kind, Error::SyncFailed(_)));
        assert_eq!(core.durable_lsn(), 0);
        assert!(log.lock().unwrap().published.is_empty());
        assert_eq!(core.scan(0).count(), 0);
        let reply = enqueue(&mut core, Kind::Turn, Durability::Durable);
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
    let (tape, _) =
        Tape::open_with(Machine::default(), disk.clone(), clock.clone(), "db", options()).unwrap();
    for _ in 0..5 {
        tape.append(vec![input(Kind::Turn, &[0; 8])], Durability::Durable).unwrap();
    }
    let all = tape.scan(0).collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(tape.scan(3).collect::<Result<Vec<_>>>().unwrap(), all[2..]);
    assert_eq!(tape.scan(6).count(), 0);
    assert_eq!(tape.scan(u64::MAX).count(), 0);
    {
        let snapshot = tape.scan(0);
        tape.append(vec![input(Kind::Turn, &[0; 8])], Durability::Durable).unwrap();
        assert_eq!(snapshot.collect::<Result<Vec<_>>>().unwrap(), all);
    }
    drop(tape);
    for watermark in [0, 3, 6, u64::MAX] {
        let machine = Machine { seq: watermark.min(6), ..Machine::default() };
        let log = machine.log.clone();
        let opts = TapeOptions { replay_from_lsn: watermark, ..options() };
        let (tape, _) = Tape::open_with(machine, disk.clone(), clock.clone(), "db", opts).unwrap();
        assert_eq!(
            log.lock().unwrap().replayed.iter().map(|r| r.lsn).collect::<Vec<_>>(),
            (1..=6).filter(|lsn| *lsn > watermark).collect::<Vec<_>>()
        );
        assert!(log.lock().unwrap().published.is_empty());
        drop(tape);
    }
    let (tape, _) = Tape::open_with(
        Machine { seq: 3, ..Machine::default() },
        disk,
        clock,
        "db",
        TapeOptions { replay_from_lsn: 3, ..options() },
    )
    .unwrap();
    tape.append(vec![input(Kind::Turn, &[0; 8])], Durability::Durable).unwrap();
    assert_eq!(tape.scan(7).next().unwrap().unwrap().payload.as_ref(), &7u64.to_le_bytes());
}

#[test]
fn replay_error_aborts_open_and_releases_the_lock() {
    let disk = Arc::new(SimDisk::new(5));
    let clock = Arc::new(SimClock::default());
    let (tape, _) = Tape::open(disk.clone(), clock.clone(), "db", options()).unwrap();
    tape.append(vec![input(Kind::Turn, &[0; 8])], Durability::Durable).unwrap();
    drop(tape);
    assert!(matches!(
        Tape::open_with(
            Machine { fail_replay: true, ..Machine::default() },
            disk.clone(),
            clock.clone(),
            "db",
            options()
        ),
        Err(Error::Configuration(_))
    ));
    Tape::open(disk, clock, "db", options()).unwrap();
}

#[test]
fn failed_sync_survivors_replay_only_after_recovery_successfully_syncs() {
    for seed in 0..32 {
        let disk = Arc::new(SimDisk::new(seed));
        let clock = Arc::new(SimClock::default());
        let machine = Machine::default();
        let log = machine.log.clone();
        let (mut core, _) =
            WriterCore::open_with(machine, disk.clone(), clock.clone(), "db", options()).unwrap();
        let reply = enqueue(&mut core, Kind::Turn, Durability::Durable);
        disk.fail_next_sync().unwrap();
        core.step();
        assert_eq!(reply.recv().unwrap().unwrap_err().outcome, Outcome::Unknown);
        assert!(log.lock().unwrap().published.is_empty());
        drop(core);

        let machine = Machine::default();
        let log = machine.log.clone();
        disk.fail_next_sync().unwrap();
        assert!(Tape::open_with(machine, disk.clone(), clock.clone(), "db", options()).is_err());
        assert!(log.lock().unwrap().replayed.is_empty());

        let machine = Machine::default();
        let log = machine.log.clone();
        let (tape, report) =
            Tape::open_with(machine, disk.clone(), clock.clone(), "db", options()).unwrap();
        assert_eq!(report.durable_lsn, 1);
        assert_eq!(log.lock().unwrap().replayed, tape.scan(0).collect::<Result<Vec<_>>>().unwrap());
        tape.append(vec![input(Kind::Turn, &[0; 8])], Durability::Durable).unwrap();
        assert_eq!(tape.scan(2).next().unwrap().unwrap().payload.as_ref(), &2u64.to_le_bytes());
        drop(tape);
        let (tape, _) =
            Tape::open(Arc::new(disk.crash().unwrap()), clock, "db", options()).unwrap();
        assert_eq!(tape.scan(0).count(), 2);
    }
}

#[derive(Debug)]
struct InvalidRewrite;

impl StateMachine for InvalidRewrite {
    fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare {
        let record = batch.records_mut().next().unwrap();
        match record.content.as_slice() {
            b"footer" => record.kind = Kind::Footer,
            b"durable" => record.kind = Kind::Turn,
            _ => record.content = vec![0; 4 * 1024 * 1024],
        }
        Prepare::Write
    }

    fn publish(&mut self, _records: &[CommittedRecord]) {
        panic!("invalid rewrite must not publish");
    }

    fn replay(&mut self, _record: &CommittedRecord) -> Result<()> {
        Ok(())
    }
}

#[test]
fn rewritten_batches_are_revalidated_without_exposing_reserved_state() {
    for (content, mode) in [
        (&b"footer"[..], Durability::Durable),
        (&b"durable"[..], Durability::Fast),
        (&b"too large"[..], Durability::Durable),
    ] {
        let (tape, _) = Tape::open_with(
            InvalidRewrite,
            Arc::new(SimDisk::new(9)),
            Arc::new(SimClock::default()),
            "db",
            options(),
        )
        .unwrap();
        let kind = if mode == Durability::Fast { Kind::Cache } else { Kind::Turn };
        assert_eq!(
            tape.append(vec![input(kind, content)], mode).unwrap_err().outcome,
            Outcome::NotCommitted
        );
        assert_eq!(tape.durable_lsn(), 0);
        assert_eq!(tape.scan(0).count(), 0);
        assert!(matches!(
            tape.append(vec![input(Kind::Turn, b"next")], Durability::Durable).unwrap_err().kind,
            Error::Poisoned
        ));
    }
}

#[test]
fn scan_skips_husks_and_footers_and_handles_gaps_and_maximum_lsn() {
    use zerodb_tape::sim::frame::{self, Footer, Frame, Header, FLAG_HUSK, VERSION};
    use zerodb_tape::sim::DataDir;
    use zerodb_tape::Io;

    let disk = Arc::new(SimDisk::new(6));
    let mut dir = DataDir::open(disk.clone(), "db", None, 1024).unwrap();
    let segment = dir.prepare_next_segment(1024).unwrap();
    let mut bytes = Vec::new();
    for (i, (lsn, flags)) in [(1, FLAG_HUSK), (8, 0), (u64::MAX, 0)].into_iter().enumerate() {
        let header = Header {
            kind: Kind::Cache,
            version: VERSION,
            flags,
            txn: i as u64 + 1,
            lsn,
            commit_time_ns: 9,
            tenant_id: 7,
            tenant_prev_chain: [0; 32],
            content_digest: frame::content_digest(&[0; 16], b"hello"),
        };
        let payload = if flags == FLAG_HUSK { vec![] } else { [&[0; 16][..], b"hello"].concat() };
        bytes.extend(frame::encode(&Frame { header: header.clone(), payload }).unwrap());
        let footer = Footer {
            txn: header.txn,
            record_count: 1,
            batch_digest: frame::batch_digest(&[header.content_digest]),
        };
        bytes.extend(
            frame::encode(&Frame {
                header: Header {
                    kind: Kind::Footer,
                    flags: 0,
                    tenant_id: 0,
                    content_digest: [0; 32],
                    ..header
                },
                payload: footer.encode().to_vec(),
            })
            .unwrap(),
        );
    }
    let file = disk.open(&dir.segment_path(segment)).unwrap();
    disk.write_at(file, 0, &bytes).unwrap();
    disk.sync_data(file).unwrap();
    disk.close(file).unwrap();
    dir.install_superblock(u64::MAX).unwrap();
    drop(dir);
    let (tape, _) = Tape::open(
        disk,
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { segment_size: 1024, ..TapeOptions::default() },
    )
    .unwrap();
    let records = tape.scan(0).collect::<Result<Vec<_>>>().unwrap();
    assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![8, u64::MAX]);
    assert!(records.iter().all(|r| r.payload.as_ref() == b"hello"));
    let mut scan = tape.scan(u64::MAX);
    assert_eq!(scan.next().unwrap().unwrap().lsn, u64::MAX);
    assert!(scan.next().is_none());
    assert!(scan.next().is_none());
}

#[test]
fn scan_reports_corruption_and_stops_after_the_bad_frame() {
    use zerodb_tape::Io;
    let disk = Arc::new(SimDisk::new(7));
    let (tape, _) =
        Tape::open(disk.clone(), Arc::new(SimClock::default()), "db", options()).unwrap();
    for _ in 0..3 {
        tape.append(vec![input(Kind::Turn, &[0; 8])], Durability::Durable).unwrap();
    }
    let file = disk.open(std::path::Path::new("db/seg-0000000000000002.zdb")).unwrap();
    disk.flip_bit(file, 0).unwrap();
    disk.close(file).unwrap();
    let mut scan = tape.scan(0);
    assert_eq!(scan.next().unwrap().unwrap().lsn, 1);
    assert!(matches!(scan.next().unwrap(), Err(Error::CorruptionDetected { lsn: 2, .. })));
    assert!(scan.next().is_none());
}
