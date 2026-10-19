use crate::support::disk_full_fixtures::*;
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

mod segments {
    use std::path::Path;
    use std::sync::Arc;
    use zerodb_tape::sim::{DataDir, SegmentWriter};
    use zerodb_tape::Error;
    use zerodb_tape::{Io, SimDisk};

    #[test]
    fn roll_prepares_names_and_syncs_every_dirty_file() {
        for seed in 0..100 {
            let io = Arc::new(SimDisk::new(seed));
            let dir = DataDir::open(io.clone(), "db", None, 512).unwrap();
            let mut writer = SegmentWriter::new(dir, 512, 0).unwrap();
            assert_eq!(writer.write_batch(&[7; 300]).unwrap(), (1, 0));
            assert_eq!(writer.write_batch(&[8; 300]).unwrap(), (2, 0));
            writer.sync().unwrap();
            drop(writer);
            let crash = io.crash().unwrap();
            let dir = DataDir::open(Arc::new(crash.clone()), "db", None, 512).unwrap();
            assert_eq!(dir.segments(), &[1, 2]);
            for (id, byte) in [(1, 7), (2, 8)] {
                let file = crash.open(&dir.segment_path(id)).unwrap();
                assert_eq!(crash.len(file).unwrap(), 512);
                let mut bytes = [0; 512];
                crash.read_at(file, 0, &mut bytes).unwrap();
                crash.close(file).unwrap();
                assert_eq!(&bytes[..300], &[byte; 300]);
                assert_eq!(&bytes[300..], &[0; 212]);
            }
        }
    }

    #[test]
    fn reject_oversized_transaction_without_rolling() {
        let io = Arc::new(SimDisk::new(0));
        let dir = DataDir::open(io.clone(), "db", None, 512).unwrap();
        let mut writer = SegmentWriter::new(dir, 512, 0).unwrap();
        assert!(matches!(writer.write_batch(&[0; 513]), Err(Error::BatchTooLarge)));
        assert!(!io.exists(Path::new("db/seg-0000000000000002.zdb")).unwrap());
    }

    #[test]
    fn preparation_failure_does_not_switch_manifest() {
        let io = Arc::new(SimDisk::new(0));
        let mut dir = DataDir::open(io.clone(), "db", None, 512).unwrap();
        dir.prepare_next_segment(512).unwrap();
        io.fail_next_sync().unwrap();
        assert!(dir.prepare_next_segment(512).is_err());
        assert_eq!(dir.segments(), &[1]);
    }
}

mod faults {
    use crate::support::simdisk_fixtures::*;
    #[test]
    fn selected_bit_flip_changes_only_that_durable_bit() {
        for bit in 0..8 {
            let disk = SimDisk::new(0);
            let file = disk.create(std::path::Path::new("bits")).unwrap();
            disk.write_at(file, 0, &[0]).unwrap();
            disk.sync_data(file).unwrap();
            disk.sync_dir(std::path::Path::new(".")).unwrap();
            disk.flip_bit_at(file, 0, bit).unwrap();
            let mut byte = [0];
            disk.read_at(file, 0, &mut byte).unwrap();
            assert_eq!(byte[0], 1 << bit);
            assert!(disk.flip_bit_at(file, 0, 8).is_err());
            disk.crash().unwrap().read_at(file, 0, &mut byte).unwrap();
            assert_eq!(byte[0], 1 << bit);
        }
    }

    use std::io::ErrorKind;
    use std::path::Path;
    use std::sync::Arc;
    use zerodb_tape::Error;
    use zerodb_tape::{Io, SimDisk};

    #[test]
    fn sparse_extension_charges_only_written_blocks_and_survives_crash() {
        let disk = SimDisk::new(17);
        disk.set_capacity(1024).unwrap();
        let file = disk.create(Path::new("db/sparse")).unwrap();
        disk.set_len(file, 4096).unwrap();
        assert_eq!(disk.len(file).unwrap(), 4096);
        assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 1024);
        disk.write_at(file, 3584, b"last block").unwrap();
        assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 512);
        disk.write_at(file, 0, b"first block").unwrap();
        assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 0);
        disk.write_at(file, 3590, b"still reserved").unwrap();
        assert!(matches!(
            disk.write_at(file, 512, b"new block"),
            Err(Error::Io(ErrorKind::StorageFull, _))
        ));
        disk.sync_data(file).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
        let image = disk.crash().unwrap();
        assert_eq!(image.available_bytes(Path::new("db")).unwrap(), 0);
        assert_eq!(image.len(file).unwrap(), 4096);
        disk.set_len(file, 512).unwrap();
        assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 512);
        disk.set_len(file, 4096).unwrap();
        assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 512);
        assert!(matches!(disk.allocate(file, 4096), Err(Error::Io(ErrorKind::StorageFull, _))));
        disk.set_capacity(4096).unwrap();
        disk.allocate(file, 4096).unwrap();
        assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 0);
        disk.write_at(file, 2048, b"reserved hole").unwrap();
    }

    #[test]
    fn synced_data_survives_1000_crash_seeds() {
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", b"durable");
            disk.write_at(file, 7, b"unsynced tail").unwrap();
            let crashed = disk.crash().unwrap();
            let file = crashed.open(Path::new("db/data")).unwrap();
            assert!(bytes(&crashed, file).starts_with(b"durable"), "seed {seed}");
        }
    }

    #[test]
    fn unsynced_writes_are_lost_torn_and_retained() {
        let mut outcomes = [false; 3];
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", b"");
            disk.write_at(file, 0, b"pending").unwrap();
            let crashed = disk.crash().unwrap();
            match bytes(&crashed, file).len() {
                0 => outcomes[0] = true,
                7 => outcomes[2] = true,
                _ => outcomes[1] = true,
            }
        }
        assert_eq!(outcomes, [true; 3]);
    }

    #[test]
    fn rename_crashes_are_atomic_for_1000_seeds() {
        let mut names = [false; 2];
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            durable_file(&disk, "db/old", b"contents");
            disk.rename(Path::new("db/old"), Path::new("db/new")).unwrap();
            let crashed = disk.crash().unwrap();
            let old = crashed.exists(Path::new("db/old")).unwrap();
            let new = crashed.exists(Path::new("db/new")).unwrap();
            assert_ne!(old, new, "seed {seed}");
            names[usize::from(new)] = true;
        }
        assert_eq!(names, [true; 2]);
    }

    #[test]
    fn failed_sync_keeps_cached_data_without_retry_healing() {
        let mut lost = false;
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", b"old");
            disk.write_at(file, 0, b"new").unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err());
            assert_eq!(bytes(&disk, file), b"new");
            disk.sync_data(file).unwrap();
            lost |= bytes(&disk.crash().unwrap(), file) != b"new";
        }
        assert!(lost, "retrying a failed sync must not heal its writes");
    }

    #[test]
    fn live_reopen_keeps_failed_cache_but_same_byte_rewrite_persists_it() {
        let mut unhealed = 0;
        for seed in 0..200 {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", b"old");
            disk.write_at(file, 0, b"new").unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err());
            disk.close(file).unwrap();
            let reopened = disk.open(Path::new("db/data")).unwrap();
            assert_eq!(bytes(&disk, reopened), b"new");
            disk.sync_data(reopened).unwrap();
            unhealed += usize::from(bytes(&disk.crash().unwrap(), reopened) != b"new");
            let cached = bytes(&disk, reopened);
            disk.write_at(reopened, 0, &cached).unwrap();
            disk.sync_data(reopened).unwrap();
            assert_eq!(bytes(&disk.crash().unwrap(), reopened), b"new");
        }
        assert!(unhealed > 0, "a live reopen and sync alone must not heal failed bytes");
    }

    #[test]
    fn later_writes_do_not_resurrect_failed_sync_bytes() {
        let mut lost = false;
        for seed in 0..100 {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", b"oldold");
            disk.write_at(file, 0, b"bad").unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err());
            disk.sync_data(file).unwrap();
            disk.write_at(file, 3, b"new").unwrap();
            disk.sync_data(file).unwrap();
            assert_eq!(bytes(&disk, file), b"badnew");
            let crash = bytes(&disk.crash().unwrap(), file);
            assert_eq!(&crash[3..], b"new");
            lost |= &crash[..3] != b"bad";
        }
        assert!(lost);
    }

    #[test]
    fn same_seed_replays_the_same_image() {
        fn run(seed: u64) -> Vec<u8> {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", b"base");
            for offset in 0..20 {
                disk.write_at(file, offset, &[offset as u8; 8]).unwrap();
            }
            bytes(&disk.crash().unwrap(), file)
        }
        for seed in 0..1000 {
            assert_eq!(run(seed), run(seed), "seed {seed}");
        }
    }

    #[test]
    fn unsynced_overwrites_can_reorder() {
        let mut reordered = false;
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", b"0000");
            disk.write_at(file, 0, b"1111").unwrap();
            disk.write_at(file, 0, b"2222").unwrap();
            let result = bytes(&disk.crash().unwrap(), file);
            reordered |= result == b"1122";
        }
        assert!(reordered);
    }

    #[test]
    fn a_later_sector_can_survive_without_the_first_sector() {
        let mut non_prefix = false;
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            let file = durable_file(&disk, "db/data", &[0; 1536]);
            disk.write_at(file, 0, &[7; 1536]).unwrap();
            let image = bytes(&disk.crash().unwrap(), file);
            non_prefix |= image[..512].iter().all(|byte| *byte == 0) && image[512..].contains(&7);
        }
        assert!(non_prefix, "sector loss must allow holes before persisted sectors");
    }

    #[test]
    fn sector_size_is_configurable_and_cannot_be_zero() {
        let mut non_prefix = false;
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            assert!(disk.set_sector_size(0).is_err());
            disk.set_sector_size(4).unwrap();
            let file = durable_file(&disk, "db/data", &[0; 12]);
            disk.write_at(file, 2, &[7; 10]).unwrap();
            let image = bytes(&disk.crash().unwrap(), file);
            non_prefix |= image[..4] == [0; 4] && image[4..].contains(&7);
        }
        assert!(non_prefix);
    }

    #[test]
    fn file_and_directory_sync_are_independent() {
        let mut lost = false;
        let mut retained = false;
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            let file = disk.create(Path::new("db/data")).unwrap();
            disk.write_at(file, 0, b"data").unwrap();
            disk.sync_data(file).unwrap();
            let crashed = disk.crash().unwrap();
            let present = crashed.exists(Path::new("db/data")).unwrap();
            lost |= !present;
            retained |= present;
            if present {
                assert_eq!(bytes(&crashed, file), b"data");
            }
        }
        assert!(lost && retained);
    }

    #[test]
    fn enospc_writes_a_prefix_and_bit_flips_damage_durable_data() {
        let disk = SimDisk::new(9);
        let file = durable_file(&disk, "db/data", b"");
        disk.enospc_after_bytes(3).unwrap();
        assert!(matches!(
            disk.write_at(file, 0, b"abcdef"),
            Err(Error::Io(ErrorKind::StorageFull, _))
        ));
        assert_eq!(bytes(&disk, file), b"abc");
        disk.sync_data(file).unwrap();
        disk.flip_bit(file, 1).unwrap();
        assert_eq!(bytes(&disk.crash().unwrap(), file), b"acc");
    }

    #[test]
    fn handles_lengths_directory_scope_and_locks() {
        let disk = Arc::new(SimDisk::new(19));
        let io: Arc<dyn Io> = disk.clone();
        let file = io.create(Path::new("db/data")).unwrap();
        assert!(io.create(Path::new("db/data")).is_err());
        io.set_len(file, 16).unwrap();
        io.write_at(file, 4, b"four").unwrap();
        io.sync_data(file).unwrap();
        io.sync_dir(Path::new("db")).unwrap();
        assert_eq!(bytes(&disk.crash().unwrap(), file).len(), 16);
        io.set_len(file, 6).unwrap();
        io.sync_data(file).unwrap();
        assert_eq!(bytes(&disk.crash().unwrap(), file), b"\0\0\0\0fo");
        assert_eq!(io.read_at(file, 100, &mut [0; 4]).unwrap(), 0);
        let guard = io.try_lock_exclusive(Path::new("db/LOCK")).unwrap();
        assert!(matches!(
            io.try_lock_exclusive(Path::new("db/LOCK")),
            Err(Error::DataDirLocked { .. })
        ));
        drop(guard);
        assert!(io.try_lock_exclusive(Path::new("db/LOCK")).is_ok());
        io.remove(Path::new("db/data")).unwrap();
        assert_eq!(bytes(io.as_ref(), file), b"\0\0\0\0fo");
        io.sync_dir(Path::new("db")).unwrap();
        assert!(!disk.crash().unwrap().exists(Path::new("db/data")).unwrap());
    }

    #[test]
    fn destination_directory_sync_persists_cross_directory_rename() {
        let disk = SimDisk::new(31);
        let file = disk.create(Path::new("source/data")).unwrap();
        disk.write_at(file, 0, b"contents").unwrap();
        disk.sync_data(file).unwrap();
        disk.rename(Path::new("source/data"), Path::new("db/data")).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
        let crashed = disk.crash().unwrap();
        let file = crashed.open(Path::new("db/data")).unwrap();
        assert_eq!(bytes(&crashed, file), b"contents");
    }

    #[test]
    fn rename_preserves_the_locked_inode() {
        let disk = SimDisk::new(0);
        let guard = disk.try_lock_exclusive(Path::new("db/LOCK")).unwrap();
        disk.rename(Path::new("db/LOCK"), Path::new("db/MOVED")).unwrap();
        assert!(matches!(
            disk.try_lock_exclusive(Path::new("db/MOVED")),
            Err(Error::DataDirLocked { .. })
        ));
        drop(guard);
        assert!(disk.try_lock_exclusive(Path::new("db/MOVED")).is_ok());
    }

    mod crash_semantics {
        use std::path::Path;
        use zerodb_tape::{Io, SimDisk};

        #[test]
        fn implicit_directory_stays_listable_when_empty() {
            let disk = SimDisk::new(0);
            assert!(disk.list(Path::new(".")).unwrap().is_empty());
            let file = disk.create(Path::new("db/file")).unwrap();
            disk.close(file).unwrap();
            disk.remove(Path::new("db/file")).unwrap();
            assert!(disk.list(Path::new("db")).unwrap().is_empty());
        }

        #[test]
        fn bit_flip_preserves_newer_cached_writes_after_failed_sync() {
            let disk = SimDisk::new(1);
            let file = disk.create(Path::new("db/file")).unwrap();
            disk.write_at(file, 0, b"oldold").unwrap();
            disk.sync_data(file).unwrap();
            disk.write_at(file, 0, b"bad").unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err());
            disk.write_at(file, 0, b"NEW").unwrap();
            disk.sync_data(file).unwrap();
            disk.flip_bit(file, 5).unwrap();
            let mut bytes = [0; 6];
            disk.read_at(file, 0, &mut bytes).unwrap();
            assert_eq!(&bytes[..3], b"NEW");
            assert_eq!(bytes[5], b'd' ^ 1);
        }

        #[test]
        fn failed_write_fates_cannot_overwrite_later_synced_data() {
            for seed in 0..1000 {
                let disk = SimDisk::new(seed);
                let file = disk.create(Path::new("db/file")).unwrap();
                disk.write_at(file, 0, &[0; 1024]).unwrap();
                disk.sync_data(file).unwrap();
                disk.sync_dir(Path::new("db")).unwrap();
                disk.set_len(file, 16).unwrap();
                disk.write_at(file, 0, &[9; 1024]).unwrap();
                disk.fail_next_sync().unwrap();
                assert!(disk.sync_data(file).is_err());
                disk.set_len(file, 1024).unwrap();
                disk.write_at(file, 0, &[7; 1024]).unwrap();
                disk.sync_data(file).unwrap();
                let crash = disk.crash().unwrap();
                let mut bytes = [0; 1024];
                assert_eq!(crash.read_at(file, 0, &mut bytes).unwrap(), 1024, "seed={seed}");
                assert_eq!(bytes, [7; 1024], "seed={seed}");
            }
        }

        #[test]
        fn failed_sync_settles_torn_bytes_without_retry_healing() {
            let mut survived = false;
            let mut lost = false;
            let mut torn = false;
            for seed in 0..200 {
                let disk = SimDisk::new(seed);
                let file = disk.create(Path::new("db/file")).unwrap();
                disk.write_at(file, 0, &[0; 1024]).unwrap();
                disk.sync_data(file).unwrap();
                disk.sync_dir(Path::new("db")).unwrap();
                disk.write_at(file, 0, &[9; 1024]).unwrap();
                disk.fail_next_sync().unwrap();
                assert!(disk.sync_data(file).is_err());
                let settled = crate::support::simdisk_fixtures::bytes(&disk.crash().unwrap(), file);

                disk.sync_data(file).unwrap();
                assert_eq!(
                    crate::support::simdisk_fixtures::bytes(&disk.crash().unwrap(), file),
                    settled
                );
                disk.flip_bit(file, 1023).unwrap();
                let mut live = [0; 1024];
                disk.read_at(file, 0, &mut live).unwrap();
                assert_eq!(live, [9; 1024]);
                let crash = disk.crash().unwrap();
                let mut bytes = [0; 1024];
                crash.read_at(file, 0, &mut bytes).unwrap();
                let mut expected = settled;
                expected[1023] ^= 1;
                assert_eq!(bytes.as_slice(), expected);
                survived |= bytes.contains(&9);
                lost |= !bytes.contains(&9);
                torn |= bytes.contains(&9) && bytes.contains(&0);
            }
            assert!(survived && lost && torn);
        }

        #[test]
        fn closed_unlinked_files_are_collected_after_directory_sync() {
            let disk = SimDisk::new(0);
            let path = Path::new("db/data");
            let file = disk.create(path).unwrap();
            disk.write_at(file, 0, &[7; 1024]).unwrap();
            disk.sync_data(file).unwrap();
            disk.sync_dir(Path::new("db")).unwrap();
            disk.remove(path).unwrap();
            disk.close(file).unwrap();
            disk.sync_dir(Path::new("db")).unwrap();
            assert_eq!(
                disk.available_bytes(Path::new("db")).unwrap(),
                disk.total_bytes(Path::new("db")).unwrap()
            );
            let crash = disk.crash().unwrap();
            assert!(!crash.exists(path).unwrap());
            assert_eq!(
                crash.available_bytes(Path::new("db")).unwrap(),
                crash.total_bytes(Path::new("db")).unwrap()
            );
        }

        #[test]
        fn crash_discards_unlinked_files_even_when_still_open() {
            let disk = SimDisk::new(0);
            let path = Path::new("db/data");
            let file = disk.create(path).unwrap();
            disk.write_at(file, 0, &[7; 1024]).unwrap();
            disk.sync_data(file).unwrap();
            disk.sync_dir(Path::new("db")).unwrap();
            disk.remove(path).unwrap();
            disk.sync_dir(Path::new("db")).unwrap();
            assert_eq!(disk.len(file).unwrap(), 1024);
            let crash = disk.crash().unwrap();
            assert!(!crash.exists(path).unwrap());
            assert_eq!(
                crash.available_bytes(Path::new("db")).unwrap(),
                crash.total_bytes(Path::new("db")).unwrap()
            );
        }
    }
}
