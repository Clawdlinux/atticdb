#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use zerodb_tape::sim::DataDir;
use zerodb_tape::sim::{SimClock, WriteRequest, WriterCore};
use zerodb_tape::{
    Committed, CommittedRecord, Durability, Kind, Outcome, Prepare, PreparedBatch, RecordInput,
    Result, StateMachine, TapeOptions,
};
use zerodb_tape::{Error, Io, SimDisk};

struct Machine(Arc<AtomicUsize>);
impl StateMachine for Machine {
    fn prepare(&mut self, _: &mut PreparedBatch) -> Prepare {
        self.0.fetch_add(1, Ordering::SeqCst);
        Prepare::Write
    }
    fn publish(&mut self, _: &[CommittedRecord]) {}
    fn replay(&mut self, _: &CommittedRecord) -> Result<()> {
        Ok(())
    }
}

fn queue(core: &mut WriterCore<Machine>) -> std::sync::mpsc::Receiver<zerodb_tape::WriteResult> {
    queue_size(core, 8)
}

fn queue_size(
    core: &mut WriterCore<Machine>,
    content_len: usize,
) -> std::sync::mpsc::Receiver<zerodb_tape::WriteResult> {
    let batch = PreparedBatch::new(
        vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![7; content_len] }],
        Durability::Durable,
    )
    .unwrap();
    let (request, reply) = WriteRequest::new(batch);
    core.enqueue(request);
    reply
}

#[test]
fn clean_floor_rejects_whole_group_before_prepare_and_recovers() {
    let disk = Arc::new(SimDisk::new(3));
    let prepares = Arc::new(AtomicUsize::new(0));
    let opts =
        TapeOptions { segment_size: 200, free_space_floor: Some(100), ..TapeOptions::default() };
    let clock = Arc::new(SimClock::default());
    let (mut core, _) = WriterCore::open_with(
        Machine(prepares.clone()),
        disk.clone(),
        clock.clone(),
        "db",
        opts.clone(),
    )
    .unwrap();
    let first = queue(&mut core);
    core.step();
    let Committed::Durable { txn, last_lsn, .. } = first.recv().unwrap().unwrap();
    assert_eq!((txn, last_lsn), (1, 1));
    let used =
        disk.total_bytes(Path::new("db")).unwrap() - disk.available_bytes(Path::new("db")).unwrap();
    // Two 120-byte batches require two fresh segments after the first write.
    disk.set_capacity(used + 100 + 240 + 400 - 1).unwrap();
    let before = disk.list(Path::new("db")).unwrap();
    let replies = [queue(&mut core), queue(&mut core)];
    core.step();
    for reply in replies {
        let error = reply.recv().unwrap().unwrap_err();
        assert!(matches!(error.kind, Error::DiskFull));
        assert_eq!(error.outcome, Outcome::NotCommitted);
    }
    assert!(core.read_only());
    assert!(!core.is_poisoned());
    assert_eq!(prepares.load(Ordering::SeqCst), 1);
    assert_eq!(core.durable_lsn(), 1);
    assert_eq!(disk.list(Path::new("db")).unwrap(), before);
    assert!(core.read(1).unwrap().is_some());
    let image = Arc::new(disk.crash().unwrap());
    let (reopened, report) =
        WriterCore::open_with(Machine(Arc::new(AtomicUsize::new(0))), image, clock, "db", opts)
            .unwrap();
    assert_eq!(report.durable_lsn, 1);
    assert_eq!(reopened.read(1).unwrap().unwrap().header.txn, 1);
    disk.set_capacity(1 << 30).unwrap();
    let retry = queue(&mut core);
    core.step();
    assert!(!core.read_only());
    let Committed::Durable { txn, last_lsn, .. } = retry.recv().unwrap().unwrap();
    assert_eq!((txn, last_lsn), (2, 2));
}

#[test]
fn default_floor_is_five_percent_capped_at_one_gib() {
    for capacity in [10000, 1 << 40] {
        let disk = Arc::new(SimDisk::new(4));
        disk.set_capacity(capacity).unwrap();
        let opts = TapeOptions { segment_size: 4096, ..TapeOptions::default() };
        let (mut core, _) = WriterCore::open_with(
            Machine(Arc::new(AtomicUsize::new(0))),
            disk.clone(),
            Arc::new(SimClock::default()),
            "db",
            opts,
        )
        .unwrap();
        let used = disk.total_bytes(Path::new("db")).unwrap()
            - disk.available_bytes(Path::new("db")).unwrap();
        let floor = (capacity / 20).min(1 << 30);
        disk.set_capacity(used + floor + 119).unwrap();
        let reply = queue(&mut core);
        core.step();
        assert!(matches!(reply.recv().unwrap().unwrap_err().kind, Error::DiskFull));
        disk.set_capacity(used + floor + 120).unwrap();
        let reply = queue(&mut core);
        core.step();
        reply.recv().unwrap().unwrap();
        assert!(!core.read_only());
    }
}
#[test]
fn a_group_spanning_segments_is_reserved_before_any_prepare() {
    struct CheckAllocation(Arc<SimDisk>, usize);
    impl StateMachine for CheckAllocation {
        fn prepare(&mut self, _: &mut PreparedBatch) -> Prepare {
            let segments = self
                .0
                .list(Path::new("db"))
                .unwrap()
                .into_iter()
                .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with("seg-"))
                .count();
            assert_eq!(segments, self.1);
            Prepare::Write
        }
        fn publish(&mut self, _: &[CommittedRecord]) {}
        fn replay(&mut self, _: &CommittedRecord) -> Result<()> {
            Ok(())
        }
    }
    let disk = Arc::new(SimDisk::new(5));
    let opts =
        TapeOptions { segment_size: 200, free_space_floor: Some(0), ..TapeOptions::default() };
    let (mut core, _) = WriterCore::open_with(
        CheckAllocation(disk.clone(), 3),
        disk.clone(),
        Arc::new(SimClock::default()),
        "db",
        opts.clone(),
    )
    .unwrap();
    let replies: Vec<_> = (0..3)
        .map(|_| {
            let batch = PreparedBatch::new(
                vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![7; 8] }],
                Durability::Durable,
            )
            .unwrap();
            let (request, reply) = WriteRequest::new(batch);
            core.enqueue(request);
            reply
        })
        .collect();
    core.step();
    for reply in replies {
        reply.recv().unwrap().unwrap();
    }
    assert_eq!(core.durable_lsn(), 3);
    drop(core);
    let (_, report) = WriterCore::open_with(
        Machine(Arc::new(AtomicUsize::new(0))),
        disk,
        Arc::new(SimClock::default()),
        "db",
        opts,
    )
    .unwrap();
    assert_eq!(report.durable_lsn, 3);
}

#[test]
fn allocation_reserves_capacity_and_space_can_return() {
    let disk = SimDisk::new(1);
    disk.set_capacity(8192).unwrap();
    let file = disk.create(Path::new("db/file")).unwrap();
    assert_eq!(disk.total_bytes(Path::new("db")).unwrap(), 8192);
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 8192);
    disk.allocate(file, 4096).unwrap();
    assert_eq!(disk.len(file).unwrap(), 4096);
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 4096);
    disk.write_at(file, 0, &[7; 4096]).unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 4096);
    assert!(matches!(
        disk.allocate(file, 9000),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(disk.len(file).unwrap(), 4096);
    disk.set_capacity(16384).unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 12288);
    disk.allocate(file, 9000).unwrap();
    disk.set_len(file, 0).unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 16384);
}

#[test]
fn growing_hooks_cannot_allocate_after_prepare() {
    struct Grow;
    impl StateMachine for Grow {
        fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare {
            for record in batch.records_mut() {
                record.content.resize(80, 7);
            }
            Prepare::Write
        }
        fn publish(&mut self, _: &[CommittedRecord]) {}
        fn replay(&mut self, _: &CommittedRecord) -> Result<()> {
            Ok(())
        }
    }
    let disk = Arc::new(SimDisk::new(6));
    let opts =
        TapeOptions { segment_size: 256, free_space_floor: Some(0), ..TapeOptions::default() };
    let (mut core, _) =
        WriterCore::open_with(Grow, disk.clone(), Arc::new(SimClock::default()), "db", opts)
            .unwrap();
    let replies: Vec<_> = (0..3)
        .map(|_| {
            let batch = PreparedBatch::new(
                vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![7; 8] }],
                Durability::Durable,
            )
            .unwrap();
            let (request, reply) = WriteRequest::new(batch);
            core.enqueue(request);
            reply
        })
        .collect();
    core.step();
    let results: Vec<_> = replies.into_iter().map(|r| r.recv().unwrap()).collect();
    assert!(results[2].is_err(), "rewrites must not allocate beyond the preflight reservation");
    assert!(core.is_poisoned());
    assert!(!disk.exists(Path::new("db/seg-0000000000000003.zdb")).unwrap());
}

#[test]
fn failed_allocation_does_not_change_manifest() {
    let disk = Arc::new(SimDisk::new(2));
    let mut dir = DataDir::open(disk.clone(), "db", None, 4096).unwrap();
    let available_before = disk.available_bytes(Path::new("db")).unwrap();
    dir.prepare_next_segment(4096).unwrap();
    assert_eq!(available_before - disk.available_bytes(Path::new("db")).unwrap(), 4096);
    let before = dir.segments().to_vec();
    let free = disk.available_bytes(Path::new("db")).unwrap();
    let total = disk.total_bytes(Path::new("db")).unwrap();
    disk.set_capacity(total - free + 4095).unwrap();
    assert!(matches!(
        dir.prepare_next_segment(4096),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(dir.segments(), before);
    drop(dir);
    disk.set_capacity(total).unwrap();
    let dir = DataDir::open(disk, "db", None, 4096).unwrap();
    assert_eq!(dir.segments(), before);
}

#[test]
fn simulated_capacity_charges_blocks_not_metadata_length_changes() {
    let disk = SimDisk::new(7);
    disk.set_capacity(1024).unwrap();
    let file = disk.create(Path::new("db/file")).unwrap();
    disk.write_at(file, 0, b"x").unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 512);
    disk.write_at(file, 511, b"x").unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 512);
    disk.write_at(file, 512, b"x").unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 0);
    assert!(matches!(
        disk.write_at(file, 1024, b"x"),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(disk.len(file).unwrap(), 513);
    disk.set_len(file, 512).unwrap();
    assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 512);
}

fn primed_writer() -> (Arc<support::ObservedIo>, WriterCore<Machine>, Arc<AtomicUsize>) {
    let io = Arc::new(support::ObservedIo::new(8));
    let prepares = Arc::new(AtomicUsize::new(0));
    let opts =
        TapeOptions { segment_size: 4096, free_space_floor: Some(100), ..TapeOptions::default() };
    let (mut core, _) = WriterCore::open_with(
        Machine(prepares.clone()),
        io.clone(),
        Arc::new(SimClock::default()),
        "db",
        opts,
    )
    .unwrap();
    let first = queue_size(&mut core, 3800);
    core.step();
    first.recv().unwrap().unwrap();
    (io, core, prepares)
}

fn manifest(disk: &SimDisk) -> Vec<u8> {
    let file = disk.open(Path::new("db/MANIFEST")).unwrap();
    let mut bytes = vec![0; disk.len(file).unwrap() as usize];
    assert_eq!(disk.read_at(file, 0, &mut bytes).unwrap(), bytes.len());
    disk.close(file).unwrap();
    bytes
}

#[test]
fn pre_roll_create_or_allocation_full_is_clean_and_leaves_no_orphan() {
    for create_failure in [false, true] {
        let (io, mut core, prepares) = primed_writer();
        let before_manifest = manifest(&io.disk);
        let before_files = io.disk.list(Path::new("db")).unwrap();
        let before_bytes = io.segment_bytes.load(Ordering::SeqCst);
        if create_failure {
            io.full_next_create.store(true, Ordering::SeqCst);
        } else {
            let used = io.disk.total_bytes(Path::new("db")).unwrap()
                - io.disk.available_bytes(Path::new("db")).unwrap();
            *io.capacity_after_space_check.lock().unwrap() = Some(used + 4095);
        }
        let replies = [queue(&mut core), queue(&mut core)];
        core.step();
        for reply in replies {
            let error = reply.recv().unwrap().unwrap_err();
            assert!(matches!(error.kind, Error::DiskFull));
            assert_eq!(error.outcome, Outcome::NotCommitted);
        }
        assert!(!core.is_poisoned());
        assert!(core.read_only());
        assert_eq!(core.durable_lsn(), 1);
        assert_eq!(prepares.load(Ordering::SeqCst), 1);
        assert_eq!(io.segment_bytes.load(Ordering::SeqCst), before_bytes);
        assert_eq!(manifest(&io.disk), before_manifest);
        assert_eq!(io.disk.list(Path::new("db")).unwrap(), before_files);
        assert!(!io.disk.exists(Path::new("db/seg-0000000000000002.zdb")).unwrap());
        io.disk.set_capacity(1 << 30).unwrap();
        let retry = queue_size(&mut core, 3800);
        core.step();
        let Committed::Durable { txn, last_lsn, .. } = retry.recv().unwrap().unwrap();
        assert_eq!((txn, last_lsn), (2, 2));
        assert!(!core.read_only());
        assert!(!core.is_poisoned());
        assert!(io.disk.exists(Path::new("db/seg-0000000000000002.zdb")).unwrap());
        assert!(!io.disk.exists(Path::new("db/seg-0000000000000003.zdb")).unwrap());
    }
}

#[test]
fn a_successful_pre_roll_stays_ready_after_later_allocation_full() {
    let (io, mut core, prepares) = primed_writer();
    let used = io.disk.total_bytes(Path::new("db")).unwrap()
        - io.disk.available_bytes(Path::new("db")).unwrap();
    *io.capacity_after_space_check.lock().unwrap() = Some(used + 8191);
    let replies = [queue_size(&mut core, 3800), queue_size(&mut core, 3800)];
    core.step();
    for reply in replies {
        let error = reply.recv().unwrap().unwrap_err();
        assert!(matches!(error.kind, Error::DiskFull));
        assert_eq!(error.outcome, Outcome::NotCommitted);
    }
    assert!(!core.is_poisoned());
    assert!(core.read_only());
    assert_eq!(prepares.load(Ordering::SeqCst), 1);
    assert!(io.disk.exists(Path::new("db/seg-0000000000000002.zdb")).unwrap());
    assert!(!io.disk.exists(Path::new("db/seg-0000000000000003.zdb")).unwrap());
    io.disk.set_capacity(1 << 30).unwrap();
    let retry = queue_size(&mut core, 3800);
    core.step();
    retry.recv().unwrap().unwrap();
    assert!(!core.read_only());
    assert!(!core.is_poisoned());
    assert_eq!(core.durable_lsn(), 2);
    assert!(!io.disk.exists(Path::new("db/seg-0000000000000003.zdb")).unwrap());
}

#[test]
fn statvfs_error_rejects_without_poisoning_or_preparing() {
    let (io, mut core, prepares) = primed_writer();
    let before_manifest = manifest(&io.disk);
    let before_bytes = io.segment_bytes.load(Ordering::SeqCst);
    io.space_error.store(true, Ordering::SeqCst);
    let replies = [queue(&mut core), queue(&mut core)];
    core.step();
    for reply in replies {
        let error = reply.recv().unwrap().unwrap_err();
        assert!(matches!(error.kind, Error::Io(std::io::ErrorKind::Other, _)));
        assert_eq!(error.outcome, Outcome::NotCommitted);
    }
    assert!(!core.is_poisoned());
    assert!(!core.read_only());
    assert_eq!(prepares.load(Ordering::SeqCst), 1);
    assert_eq!(core.durable_lsn(), 1);
    assert_eq!(io.segment_bytes.load(Ordering::SeqCst), before_bytes);
    assert_eq!(manifest(&io.disk), before_manifest);
    let retry = queue(&mut core);
    core.step();
    retry.recv().unwrap().unwrap();
    assert_eq!(core.durable_lsn(), 2);
    assert!(!core.is_poisoned());
}

#[test]
fn mixed_group_is_rejected_before_its_fitting_first_batch_is_prepared() {
    let (io, mut core, prepares) = primed_writer();
    let used = io.disk.total_bytes(Path::new("db")).unwrap()
        - io.disk.available_bytes(Path::new("db")).unwrap();
    // The first 120-byte batch fits. The second needs a 4096-byte segment.
    io.disk.set_capacity(used + 100 + 240 + 4096 - 1).unwrap();
    let before_manifest = manifest(&io.disk);
    let before_bytes = io.segment_bytes.load(Ordering::SeqCst);
    let before_events = io.events.lock().unwrap().clone();
    let replies = [queue(&mut core), queue(&mut core)];
    core.step();
    for reply in replies {
        let error = reply.recv().unwrap().unwrap_err();
        assert!(matches!(error.kind, Error::DiskFull));
        assert_eq!(error.outcome, Outcome::NotCommitted);
    }
    assert!(core.read_only());
    assert!(!core.is_poisoned());
    assert_eq!(prepares.load(Ordering::SeqCst), 1);
    assert_eq!(core.durable_lsn(), 1);
    assert_eq!(io.segment_bytes.load(Ordering::SeqCst), before_bytes);
    assert_eq!(manifest(&io.disk), before_manifest);
    assert_eq!(*io.events.lock().unwrap(), before_events);
}

#[test]
fn ambiguous_pre_roll_sync_and_manifest_failures_still_poison() {
    for fault in 0..3 {
        let (io, mut core, prepares) = primed_writer();
        match fault {
            0 => io.fail_at.store(io.syncs.load(Ordering::SeqCst) + 1, Ordering::SeqCst),
            1 => io.fail_directory_sync.store(true, Ordering::SeqCst),
            _ => io.full_manifest_write.store(true, Ordering::SeqCst),
        }
        let replies = [queue(&mut core), queue(&mut core)];
        core.step();
        for reply in replies {
            let error = reply.recv().unwrap().unwrap_err();
            assert_eq!(error.outcome, Outcome::NotCommitted);
            assert!(matches!(error.kind, Error::SyncFailed(_) | Error::DiskFull));
        }
        assert!(core.is_poisoned());
        assert_eq!(prepares.load(Ordering::SeqCst), 1);
        assert_eq!(core.durable_lsn(), 1);
    }
}
