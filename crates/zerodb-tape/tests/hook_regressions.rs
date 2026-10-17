#![allow(clippy::unwrap_used)]

use std::sync::{mpsc, Arc, Mutex};
use zerodb_tape::sim::{SimClock, WriteRequest, WriterCore};
use zerodb_tape::{
    Committed, CommittedRecord, Durability, Error, Kind, Outcome, Prepare, PreparedBatch,
    RecordInput, Result, SimDisk, StateMachine, TapeOptions, WriteResult,
};

mod support;
use support::ObservedIo;

#[derive(Debug, Default)]
struct Recording(Arc<Mutex<Vec<u64>>>);

impl StateMachine for Recording {
    fn prepare(&mut self, _batch: &mut PreparedBatch) -> Prepare {
        Prepare::Write
    }

    fn publish(&mut self, records: &[CommittedRecord]) {
        self.0.lock().unwrap().extend(records.iter().map(|record| record.lsn));
    }

    fn replay(&mut self, _record: &CommittedRecord) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Dedup {
    cached: bool,
    replies: Arc<Mutex<Vec<mpsc::Receiver<WriteResult>>>>,
    published: Arc<Mutex<Vec<u64>>>,
}

fn original() -> Committed {
    Committed::Durable { txn: 1, first_lsn: 1, last_lsn: 1 }
}

impl StateMachine for Dedup {
    fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare {
        if batch.records().next().unwrap().content == b"trailing" {
            return Prepare::Write;
        }
        if self.cached {
            Prepare::Reply(original())
        } else {
            self.cached = true;
            Prepare::Write
        }
    }

    fn publish(&mut self, records: &[CommittedRecord]) {
        for reply in self.replies.lock().unwrap().iter() {
            assert!(matches!(reply.try_recv(), Err(mpsc::TryRecvError::Empty)));
        }
        self.published.lock().unwrap().extend(records.iter().map(|record| record.lsn));
    }

    fn replay(&mut self, _record: &CommittedRecord) -> Result<()> {
        Ok(())
    }
}

fn request(content: &[u8], mode: Durability) -> (WriteRequest, mpsc::Receiver<WriteResult>) {
    let kind = if mode == Durability::Fast { Kind::Cache } else { Kind::Turn };
    let batch = PreparedBatch::with_salts(
        vec![RecordInput { kind, tenant_id: 1, content: content.to_vec() }],
        mode,
        || Ok([0; 16]),
    )
    .unwrap();
    WriteRequest::new(batch)
}

#[test]
fn same_group_retry_waits_for_sync_and_publication() {
    for fail_sync in [false, true] {
        let disk = Arc::new(SimDisk::new(0));
        let machine = Dedup::default();
        let replies = machine.replies.clone();
        let published = machine.published.clone();
        let (mut core, _) = WriterCore::open_with(
            machine,
            disk.clone(),
            Arc::new(SimClock::default()),
            "db",
            TapeOptions { segment_size: 2048, ..TapeOptions::default() },
        )
        .unwrap();
        for _ in 0..2 {
            let (request, reply) = request(b"original", Durability::Durable);
            replies.lock().unwrap().push(reply);
            core.enqueue(request);
        }
        if fail_sync {
            disk.fail_next_sync().unwrap();
        }
        core.step();
        for reply in replies.lock().unwrap().iter() {
            let result = reply.recv().unwrap();
            if fail_sync {
                let error = result.unwrap_err();
                assert!(matches!(error.kind, Error::SyncFailed(_)));
                assert_eq!(error.outcome, Outcome::Unknown);
            } else {
                assert_eq!(result.unwrap(), original());
            }
        }
        assert_eq!(core.is_poisoned(), fail_sync);
        assert_eq!(published.lock().unwrap().as_slice(), if fail_sync { &[][..] } else { &[1] });
    }
}

#[test]
fn same_group_retry_shares_the_prefix_fate_after_a_partial_write() {
    for fail_sync in [false, true] {
        let disk = Arc::new(SimDisk::new(1));
        let machine = Dedup::default();
        let replies = machine.replies.clone();
        let published = machine.published.clone();
        let (mut core, _) = WriterCore::open_with(
            machine,
            disk.clone(),
            Arc::new(SimClock::default()),
            "db",
            TapeOptions { segment_size: 2048, ..TapeOptions::default() },
        )
        .unwrap();
        for _ in 0..2 {
            let (request, reply) = request(b"original", Durability::Durable);
            replies.lock().unwrap().push(reply);
            core.enqueue(request);
        }
        let (trailing, trailing_reply) = request(b"trailing", Durability::Durable);
        core.enqueue(trailing);
        disk.enospc_after_bytes(276 + 5).unwrap();
        if fail_sync {
            disk.fail_next_sync().unwrap();
        }
        core.step();
        for reply in replies.lock().unwrap().iter() {
            let result = reply.recv().unwrap();
            if fail_sync {
                assert_eq!(result.unwrap_err().outcome, Outcome::Unknown);
            } else {
                assert_eq!(result.unwrap(), original());
            }
        }
        assert!(trailing_reply.recv().unwrap().is_err());
        assert!(core.is_poisoned());
        assert_eq!(published.lock().unwrap().as_slice(), if fail_sync { &[][..] } else { &[1] });
    }
}

#[test]
fn fast_retry_never_exposes_a_cached_durable_identifier() {
    let (mut core, _) = WriterCore::open_with(
        Dedup::default(),
        Arc::new(SimDisk::new(2)),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { segment_size: 2048, ..TapeOptions::default() },
    )
    .unwrap();
    let (first, first_reply) = request(b"original", Durability::Durable);
    core.enqueue(first);
    core.step();
    assert_eq!(first_reply.recv().unwrap().unwrap(), original());
    let (retry, reply) = request(b"original", Durability::Fast);
    core.enqueue(retry);
    core.step();
    assert_eq!(reply.recv().unwrap().unwrap(), Committed::Fast);
    assert_eq!(core.durable_lsn(), 1);
}

#[test]
fn fast_only_traffic_flushes_on_roll_and_releases_old_handles() {
    let io = Arc::new(ObservedIo::new(3));
    let machine = Recording::default();
    let published = machine.0.clone();
    let (mut core, _) = WriterCore::open_with(
        machine,
        io.clone(),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { segment_size: 512, ..TapeOptions::default() },
    )
    .unwrap();
    for n in 1..=64 {
        let (request, reply) = request(b"original", Durability::Fast);
        core.enqueue(request);
        core.step();
        assert_eq!(reply.recv().unwrap().unwrap(), Committed::Fast);
        assert!(n - core.durable_lsn() <= 1, "Fast-only buffering must stay bounded");
        assert_eq!(published.lock().unwrap().len() as u64, core.durable_lsn());
        assert_eq!(io.handles.lock().unwrap().len(), 1, "rolled segment handles must be released");
    }
    assert_eq!(core.scan(0).count(), 64);
    assert_eq!(published.lock().unwrap().as_slice(), &(1..=64).collect::<Vec<_>>());
    drop(core);
    assert!(io.handles.lock().unwrap().is_empty());
}

#[test]
fn fast_byte_limit_bounds_pending_batches_and_resets_after_publication() {
    let io = Arc::new(ObservedIo::new(4));
    let machine = Recording::default();
    let published = machine.0.clone();
    let (mut core, _) = WriterCore::open_with(
        machine,
        io.clone(),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions {
            segment_size: 8192,
            max_unpublished_bytes: Some(552),
            ..TapeOptions::default()
        },
    )
    .unwrap();
    for n in 1..=64 {
        let (request, reply) = request(b"original", Durability::Fast);
        core.enqueue(request);
        core.step();
        assert_eq!(reply.recv().unwrap().unwrap(), Committed::Fast);
        assert!((n - core.durable_lsn()) * 276 <= 552);
        if n <= 29 {
            assert_eq!(core.durable_lsn(), n / 3 * 3);
        }
        assert_eq!(published.lock().unwrap().len() as u64, core.durable_lsn());
        assert_eq!(io.handles.lock().unwrap().len(), 1);
    }
    core.shutdown().unwrap();
    assert_eq!(published.lock().unwrap().as_slice(), &(1..=64).collect::<Vec<_>>());
    drop(core);
    assert!(io.handles.lock().unwrap().is_empty());
}

#[test]
fn failed_automatic_fast_flush_never_publishes_and_poison_blocks_further_writes() {
    let disk = Arc::new(SimDisk::new(5));
    let machine = Recording::default();
    let published = machine.0.clone();
    let (mut core, _) = WriterCore::open_with(
        machine,
        disk.clone(),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions {
            segment_size: 2048,
            max_unpublished_bytes: Some(0),
            ..TapeOptions::default()
        },
    )
    .unwrap();
    let (first, reply) = request(b"original", Durability::Fast);
    core.enqueue(first);
    disk.fail_next_sync().unwrap();
    core.step();
    assert_eq!(reply.recv().unwrap().unwrap(), Committed::Fast);
    assert!(core.is_poisoned());
    assert_eq!(core.durable_lsn(), 0);
    assert!(published.lock().unwrap().is_empty());
    let (next, reply) = request(b"original", Durability::Fast);
    core.enqueue(next);
    core.step();
    assert!(matches!(reply.recv().unwrap().unwrap_err().kind, Error::Poisoned));
    assert!(matches!(core.shutdown(), Err(Error::Poisoned)));
    assert!(published.lock().unwrap().is_empty());
}

#[test]
fn failed_automatic_roll_flush_keeps_the_entire_fast_group_unpublished() {
    use std::sync::atomic::Ordering;
    let io = Arc::new(ObservedIo::new(6));
    let machine = Recording::default();
    let published = machine.0.clone();
    let (mut core, _) = WriterCore::open_with(
        machine,
        io.clone(),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions {
            segment_size: 512,
            max_unpublished_bytes: Some(u64::MAX),
            ..TapeOptions::default()
        },
    )
    .unwrap();
    let (first, first_reply) = request(b"original", Durability::Fast);
    core.enqueue(first);
    core.step();
    assert_eq!(first_reply.recv().unwrap().unwrap(), Committed::Fast);
    // The roll syncs its blank segment and manifest before syncing both dirty segments.
    io.fail_at.store(io.syncs.load(Ordering::SeqCst) + 4, Ordering::SeqCst);
    let (second, second_reply) = request(b"original", Durability::Fast);
    core.enqueue(second);
    core.step();
    assert_eq!(second_reply.recv().unwrap().unwrap(), Committed::Fast);
    assert!(core.is_poisoned());
    assert_eq!(core.durable_lsn(), 0);
    assert!(published.lock().unwrap().is_empty());
    drop(core);
    assert!(io.handles.lock().unwrap().is_empty());
}
