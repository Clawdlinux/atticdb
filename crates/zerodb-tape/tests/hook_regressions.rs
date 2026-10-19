#![allow(clippy::unwrap_used)]

use std::sync::{mpsc, Arc, Mutex};
use zerodb_tape::sim::{SimClock, WriteRequest, WriterCore};
use zerodb_tape::{
    Committed, CommittedRecord, Durability, Error, Kind, Outcome, Prepare, PreparedBatch,
    RecordInput, Result, SimDisk, StateMachine, TapeOptions, WriteResult,
};

#[derive(Debug, Default)]
struct Dedup {
    cached: bool,
    replies: Arc<Mutex<Vec<mpsc::Receiver<WriteResult>>>>,
    published: Arc<Mutex<Vec<u64>>>,
}

fn original() -> Committed {
    Committed::Durable { txn: 1, first_lsn: 1, last_lsn: 1, reply: Vec::new() }
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
    fn replay(&mut self, _: &CommittedRecord) -> Result<()> {
        Ok(())
    }
}

fn request(content: &[u8]) -> (WriteRequest, mpsc::Receiver<WriteResult>) {
    WriteRequest::new(
        PreparedBatch::new(
            vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: content.to_vec() }],
            Durability::Durable,
        )
        .unwrap(),
    )
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
            let (request, reply) = request(b"original");
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
            let (request, reply) = request(b"original");
            replies.lock().unwrap().push(reply);
            core.enqueue(request);
        }
        let (trailing, trailing_reply) = request(b"trailing");
        core.enqueue(trailing);
        disk.enospc_after_bytes(120 + 5).unwrap();
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
