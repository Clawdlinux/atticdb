use crate::support::storage::{bytes, durable_file};
use crate::support::Behavior;
use crate::support::{manifest, primed_writer, record, submit, Machine, ObservedIo};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zerodb_tape::sim::{frame::HEADER_LEN, SimClock, WriterCore};
use zerodb_tape::sim::{make_spare, DataDir};
use zerodb_tape::{Committed, Durability, Kind, Outcome, TapeOptions};
use zerodb_tape::{Error, Io, SimDisk};

#[derive(Clone, Copy)]
enum Fault {
    Floor,
    IndexFloor,
    IndexOffsets,
    IndexRoutes,
    IndexStat,
    DefaultFloor(u64),
    Create,
    Allocate,
    LaterAllocate,
    Stat,
    Sync,
    DirSync,
    Manifest,
    Spare,
}

#[test]
fn space_faults_set_group_outcome() {
    use Fault::*;
    for (case, fault, expect_read_only, expect_poisoned) in [
        ("whole group floor", Floor, true, false),
        // Capacity covers the 2 MiB index reservation at open. Five percent stays under the cap.
        ("five percent floor", DefaultFloor(64 << 20), true, false),
        ("capped default floor", DefaultFloor(1 << 40), true, false),
        ("create full", Create, true, false),
        ("allocate full", Allocate, true, false),
        ("later allocation full", LaterAllocate, true, false),
        ("space query error", Stat, true, false),
        ("segment sync error", Sync, false, true),
        ("directory sync error", DirSync, false, true),
        ("manifest write full", Manifest, false, true),
        ("spare is one prepared roll", Spare, true, false),
    ] {
        space_fault(case, fault, expect_read_only, expect_poisoned);
    }
}

// Index rows prime 131072 LSNs to cross a 1 MiB backing chunk.
#[test]
#[cfg_attr(debug_assertions, ignore = "slow in debug")]
fn index_growth_space_faults_set_group_outcome() {
    use Fault::*;
    for (case, fault, expect_read_only, expect_poisoned) in [
        ("index growth floor", IndexFloor, true, false),
        ("index offsets allocation full", IndexOffsets, true, false),
        ("index routes allocation full", IndexRoutes, true, false),
        ("index growth space query error", IndexStat, true, false),
    ] {
        space_fault(case, fault, expect_read_only, expect_poisoned);
    }
}

fn space_fault(case: &str, fault: Fault, expect_read_only: bool, expect_poisoned: bool) {
    use Fault::*;
    eprintln!("case: {case}");
    let capacity = if let DefaultFloor(capacity) = fault { capacity } else { 1 << 30 };
    let floor = if matches!(fault, DefaultFloor(_)) { None } else { Some(100) };
    let indexed = matches!(fault, IndexFloor | IndexOffsets | IndexRoutes | IndexStat);
    // Index backing grows in 1 MiB chunks. 131072 LSNs fill one offsets chunk and two routes
    // chunks exactly, so the next LSN grows both files. Large segments keep priming fast.
    let boundary = if indexed { 131072 } else { 1 };
    let segment = if indexed { 1 << 20 } else { 4096 };
    let index_lens = |disk: &SimDisk| {
        ["LSN-OFFSETS", "LSN-ROUTES"].map(|name| {
            let file = disk.open(&Path::new("db").join(name)).unwrap();
            let len = disk.len(file).unwrap();
            disk.close(file).unwrap();
            len
        })
    };
    let (io, mut core, prepares) = primed_writer(floor, capacity, boundary, segment);
    if matches!(fault, Spare) {
        make_spare(io.clone(), Path::new("db"), segment, 0).unwrap();
    }
    let before_prepares = prepares.load(Ordering::SeqCst);
    let before_manifest = manifest(&io.disk);
    let before_files = io.disk.list(Path::new("db")).unwrap();
    let next_segment = before_files
        .iter()
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("seg-"))
        .count() as u64
        + 1;
    let before_events = io.events.lock().unwrap().clone();
    if indexed {
        let allocations =
            before_events.iter().filter(|e| e.starts_with("allocate db/LSN-")).count();
        // Page-wise growth made 258 calls here, one per 1024-LSN group per file.
        assert_eq!(allocations, 3, "{case}: index allocate calls while priming");
        assert_eq!(index_lens(&io.disk), [1 << 20, 2 << 20], "{case}: primed index backing");
    }
    let before_bytes = io.segment_bytes.load(Ordering::SeqCst);
    let used = capacity - io.disk.available_bytes(Path::new("db")).unwrap();
    let chunk = 1 << 20;
    // A ready spare already holds the roll's blocks.
    let roll = if matches!(fault, Spare) { 0 } else { segment };
    let reserve = 240 + roll + if indexed { 2 * chunk } else { 0 };
    let floor_bytes = floor.unwrap_or((capacity / 20).min(1 << 30));
    match fault {
        Floor | IndexFloor | DefaultFloor(_) | Spare => {
            io.disk.set_capacity(used + floor_bytes + reserve - 1).unwrap()
        }
        Create => io.full_next_create.store(true, Ordering::SeqCst),
        Allocate | LaterAllocate | IndexOffsets | IndexRoutes => {
            // Each row leaves one byte less than its failing allocation needs.
            *io.capacity_after_space_check.lock().unwrap() = Some(
                used + match fault {
                    IndexOffsets => chunk - 1,
                    IndexRoutes => 2 * chunk - 1,
                    LaterAllocate => 8191,
                    _ => 4095,
                },
            )
        }
        Stat | IndexStat => io.space_error.store(true, Ordering::SeqCst),
        Sync => io.fail_at.store(io.syncs.load(Ordering::SeqCst) + 1, Ordering::SeqCst),
        DirSync => io.fail_directory_sync.store(true, Ordering::SeqCst),
        Manifest => io.full_manifest_write.store(true, Ordering::SeqCst),
    }
    let size = if matches!(fault, LaterAllocate) { 3800 } else { 8 };
    let replies = [
        submit(&mut core, vec![record(Kind::Turn, 1, vec![7; size])], Durability::Durable),
        submit(&mut core, vec![record(Kind::Turn, 1, vec![7; size])], Durability::Durable),
    ];
    core.step();
    for reply in replies {
        let error = reply.recv().unwrap().unwrap_err();
        assert_eq!(error.outcome, Outcome::NotCommitted, "{case}");
        assert!(
            match fault {
                Stat | IndexStat => matches!(error.kind, Error::Io(std::io::ErrorKind::Other, _)),
                Sync | DirSync => matches!(error.kind, Error::SyncFailed(_)),
                _ => matches!(error.kind, Error::DiskFull),
            },
            "{case}: {error:?}"
        );
    }
    assert_eq!(core.read_only(), expect_read_only, "{case}");
    assert_eq!(core.is_poisoned(), expect_poisoned, "{case}");
    assert_eq!(prepares.load(Ordering::SeqCst), before_prepares, "{case}");
    assert_eq!(core.durable_lsn(), boundary, "{case}");
    assert!(core.read(1).unwrap().is_some(), "{case}");
    assert_eq!(io.segment_bytes.load(Ordering::SeqCst), before_bytes, "{case}");
    if matches!(fault, LaterAllocate) {
        assert_ne!(manifest(&io.disk), before_manifest, "{case}");
    } else {
        assert_eq!(manifest(&io.disk), before_manifest, "{case}");
    }
    assert_eq!(
        io.disk.exists(&Path::new("db").join(format!("seg-{next_segment:016}.zdb"))).unwrap(),
        matches!(fault, LaterAllocate | Sync | DirSync | Manifest),
        "{case}"
    );
    assert!(
        !io.disk
            .exists(&Path::new("db").join(format!("seg-{:016}.zdb", next_segment + 1)))
            .unwrap(),
        "{case}"
    );
    if !matches!(fault, LaterAllocate | Sync | DirSync | Manifest) {
        assert_eq!(io.disk.list(Path::new("db")).unwrap(), before_files, "{case}");
    }
    if matches!(fault, Floor | IndexFloor | DefaultFloor(_) | Stat | IndexStat | Spare) {
        assert_eq!(*io.events.lock().unwrap(), before_events, "{case}");
    }
    let retry_size = match fault {
        DefaultFloor(_) => {
            io.disk.set_capacity(used + floor_bytes + 120).unwrap();
            8
        }
        // The roll fits only by taking the spare. Then a refill would cross the floor by 1 byte.
        Spare => {
            io.disk.set_capacity(used + floor_bytes + segment - 1).unwrap();
            3800
        }
        _ => {
            io.disk.set_capacity(1 << 42).unwrap();
            3800
        }
    };
    if expect_poisoned {
        let retry = submit(&mut core, vec![record(Kind::Turn, 1, [7; 8])], Durability::Durable);
        core.step();
        assert!(matches!(retry.recv().unwrap().unwrap_err().kind, Error::Poisoned), "{case}");
        drop(core);
        let opts = TapeOptions {
            segment_size: segment,
            free_space_floor: floor,
            ..TapeOptions::default()
        };
        core = WriterCore::open_with(
            Machine { prepares: prepares.clone(), ..Machine::default() },
            io.clone(),
            Arc::new(SimClock::default()),
            "db",
            opts,
        )
        .unwrap()
        .0;
        assert!(!io.disk.exists(Path::new("db/seg-0000000000000002.zdb")).unwrap(), "{case}");
    }
    let retry =
        submit(&mut core, vec![record(Kind::Turn, 1, vec![7; retry_size])], Durability::Durable);
    core.step();
    let Committed::Durable { txn, last_lsn, .. } = retry.recv().unwrap().unwrap();
    assert_eq!((txn, last_lsn), (boundary + 1, boundary + 1), "{case}");
    if indexed {
        assert_eq!(index_lens(&io.disk), [2 << 20, 3 << 20], "{case}: one chunk per file");
    }
    assert!(!core.read_only() && !core.is_poisoned(), "{case}");
    assert_eq!(prepares.load(Ordering::SeqCst), before_prepares + 1, "{case}");
    if matches!(fault, Spare) {
        core.maintain().unwrap();
        assert!(!io.disk.exists(Path::new("db/spare.zdb")).unwrap(), "{case}: refill skipped");
        // A refill would push the tail write below the floor. The next roll hits it.
        for floor_hit in [false, true] {
            let reply = submit(&mut core, vec![record(Kind::Turn, 1, [7; 8])], Durability::Durable);
            core.step();
            let full = reply.recv().unwrap().map_err(|e| matches!(e.kind, Error::DiskFull));
            assert_eq!(full.err(), floor_hit.then_some(true), "{case}");
        }
    }
    let durable = core.durable_lsn();
    drop(core);
    let image = Arc::new(io.disk.crash().unwrap());
    image.set_capacity(1 << 42).unwrap();
    let opts =
        TapeOptions { segment_size: segment, free_space_floor: floor, ..TapeOptions::default() };
    let (core, report) =
        WriterCore::open_with(Machine::default(), image, Arc::new(SimClock::default()), "db", opts)
            .unwrap();
    assert_eq!(report.durable_lsn, durable, "{case}");
    // One read per 4096-byte routes page keeps the 131072-LSN rows fast in debug builds.
    let stride = if indexed { 256 } else { 1 };
    for lsn in (1..=durable).step_by(stride).chain([durable]) {
        assert!(core.read(lsn).unwrap().is_some(), "{case}, lsn={lsn}");
    }
}

#[test]
fn simulated_disk_lifecycle_tracks_capacity_locks_and_unlink() {
    let disk = SimDisk::new(19);
    let dir = Path::new("db");
    let path = dir.join("data");
    disk.set_capacity(1024).unwrap();
    let file = disk.create(&path).unwrap();
    assert!(disk.create(&path).is_err());
    disk.set_len(file, 4096).unwrap();
    assert_eq!(disk.available_bytes(dir).unwrap(), 1024);
    for (case, offset, free) in
        [("first block", 0, 512), ("overwrite", 511, 512), ("last block", 3584, 0)]
    {
        eprintln!("case: {case}");
        disk.write_at(file, offset, b"x").unwrap();
        assert_eq!(disk.available_bytes(dir).unwrap(), free, "{case}");
    }
    assert!(matches!(
        disk.write_at(file, 512, b"x"),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert!(matches!(
        disk.allocate(file, 4096),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(disk.len(file).unwrap(), 4096);
    let mut gap = [1; 4];
    assert_eq!(disk.read_at(file, 100, &mut gap).unwrap(), 4);
    assert_eq!(gap, [0; 4]);
    assert_eq!(disk.read_at(file, 8192, &mut gap).unwrap(), 0);
    disk.set_len(file, 512).unwrap();
    assert_eq!(disk.available_bytes(dir).unwrap(), 512);
    disk.set_len(file, 4096).unwrap();
    assert_eq!(disk.available_bytes(dir).unwrap(), 512);
    disk.set_capacity(8192).unwrap();
    disk.allocate(file, 4096).unwrap();
    assert_eq!(disk.available_bytes(dir).unwrap(), 4096);
    assert!(matches!(
        disk.allocate(file, 9000),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(disk.len(file).unwrap(), 4096);
    disk.set_capacity(16384).unwrap();
    disk.allocate(file, 9000).unwrap();
    disk.set_len(file, 0).unwrap();
    assert_eq!(disk.available_bytes(dir).unwrap(), 16384);
    disk.enospc_after_bytes(3).unwrap();
    assert!(matches!(
        disk.write_at(file, 0, b"abcdef"),
        Err(Error::Io(std::io::ErrorKind::StorageFull, _))
    ));
    assert_eq!(bytes(&disk, file), b"abc");
    disk.sync_data(file).unwrap();
    disk.sync_dir(dir).unwrap();
    disk.flip_bit(file, 1).unwrap();
    assert_eq!(bytes(&disk.crash().unwrap(), file), b"acc");
    disk.rename(&path, Path::new("source/data")).unwrap();
    disk.sync_dir(Path::new("source")).unwrap();
    let image = disk.crash().unwrap();
    assert_eq!(bytes(&image, image.open(Path::new("source/data")).unwrap()), b"acc");
    disk.rename(Path::new("source/data"), &path).unwrap();
    let guard = disk.try_lock_exclusive(Path::new("db/LOCK")).unwrap();
    disk.rename(Path::new("db/LOCK"), Path::new("db/MOVED")).unwrap();
    assert!(matches!(
        disk.try_lock_exclusive(Path::new("db/MOVED")),
        Err(Error::DataDirLocked { .. })
    ));
    drop(guard);
    assert!(disk.try_lock_exclusive(Path::new("db/MOVED")).is_ok());
    disk.remove(Path::new("db/MOVED")).unwrap();
    disk.remove(&path).unwrap();
    assert_eq!(bytes(&disk, file), b"acc");
    disk.sync_dir(dir).unwrap();
    let image = disk.crash().unwrap();
    assert!(!image.exists(&path).unwrap());
    assert_eq!(image.available_bytes(dir).unwrap(), 16384);
    disk.close(file).unwrap();
    disk.sync_dir(dir).unwrap();
    assert_eq!(disk.available_bytes(dir).unwrap(), 16384);
}

#[test]
fn seeded_disk_crashes_preserve_sync_fidelity() {
    let mut fates = [false; 3];
    let mut failed_fates = [false; 3];
    let mut rename_names = [false; 2];
    let mut directory_names = [false; 2];
    let mut holes = false;
    let mut reordered = false;
    for seed in 0..1000 {
        for sector in [4, 512] {
            let case = format!("seed={seed}, sector={sector}");
            eprintln!("case: {case}");
            let disk = SimDisk::new(seed);
            disk.set_sector_size(sector).unwrap();
            let len = sector * 3;
            let mut base = vec![0; len];
            base[..sector].fill(7);
            let file = durable_file(&disk, "db/data", &base);
            disk.write_at(file, sector as u64, &vec![9; sector * 2]).unwrap();
            let image = bytes(&disk.crash().unwrap(), file);
            assert_eq!(&image[..sector], &base[..sector], "{case}");
            let tail = &image[sector..];
            fates[if !tail.contains(&9) {
                0
            } else if !tail.contains(&0) {
                2
            } else {
                1
            }] = true;
            holes |= tail[..sector].iter().all(|b| *b == 0) && tail[sector..].contains(&9);
            disk.write_at(file, 0, &vec![0; len]).unwrap();
            disk.sync_data(file).unwrap();
            disk.write_at(file, 0, &vec![9; len]).unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err(), "{case}");
            assert_eq!(bytes(&disk, file), vec![9; len], "{case}");
            let settled = bytes(&disk.crash().unwrap(), file);
            failed_fates[if !settled.contains(&9) {
                0
            } else if !settled.contains(&0) {
                2
            } else {
                1
            }] = true;
            disk.sync_data(file).unwrap();
            assert_eq!(bytes(&disk.crash().unwrap(), file), settled, "{case}: sync must not heal");
            disk.close(file).unwrap();
            let file = disk.open(Path::new("db/data")).unwrap();
            assert_eq!(bytes(&disk, file), vec![9; len], "{case}: live reopen keeps cache");
            disk.sync_data(file).unwrap();
            assert_eq!(
                bytes(&disk.crash().unwrap(), file),
                settled,
                "{case}: reopen must not heal"
            );
            disk.write_at(file, (sector * 2) as u64, &vec![7; sector]).unwrap();
            disk.sync_data(file).unwrap();
            let image = bytes(&disk.crash().unwrap(), file);
            assert_eq!(
                &image[..sector * 2],
                &settled[..sector * 2],
                "{case}: disjoint writes must not resurrect failures"
            );
            assert_eq!(&image[sector * 2..], &vec![7; sector], "{case}");
            disk.flip_bit(file, (len - 1) as u64).unwrap();
            let cached = bytes(&disk, file);
            assert_eq!(cached[len - 1], 6, "{case}: clean durable bit flip");
            assert_eq!(
                &cached[..sector * 2],
                &vec![9; sector * 2],
                "{case}: bit flip keeps newer cache"
            );
            disk.write_at(file, 0, &cached).unwrap();
            disk.sync_data(file).unwrap();
            assert_eq!(
                bytes(&disk.crash().unwrap(), file),
                cached,
                "{case}: identical rewrite persists"
            );
            disk.set_len(file, (len / 2) as u64).unwrap();
            disk.write_at(file, 0, &vec![9; len]).unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err(), "{case}");
            disk.set_len(file, len as u64).unwrap();
            disk.write_at(file, 0, &vec![7; len]).unwrap();
            disk.sync_data(file).unwrap();
            assert_eq!(
                bytes(&disk.crash().unwrap(), file),
                vec![7; len],
                "{case}: stale failed fates cannot overwrite later syncs"
            );
            disk.rename(Path::new("db/data"), Path::new("db/new")).unwrap();
            let image = disk.crash().unwrap();
            let old = image.exists(Path::new("db/data")).unwrap();
            let new = image.exists(Path::new("db/new")).unwrap();
            assert_ne!(old, new, "{case}: rename is atomic");
            rename_names[usize::from(new)] = true;
            disk.sync_dir(Path::new("db")).unwrap();
            let other = disk.create(Path::new("db/unsynced")).unwrap();
            disk.write_at(other, 0, b"data").unwrap();
            disk.sync_data(other).unwrap();
            let image = disk.crash().unwrap();
            let present = image.exists(Path::new("db/unsynced")).unwrap();
            directory_names[usize::from(present)] = true;
            if present {
                assert_eq!(bytes(&image, other), b"data", "{case}: data sync is independent");
            }
            let overlap = durable_file(&disk, "db/overlap", b"0000");
            disk.write_at(overlap, 0, b"1111").unwrap();
            disk.write_at(overlap, 0, b"2222").unwrap();
            reordered |= bytes(&disk.crash().unwrap(), overlap) == b"1122";
        }
    }
    assert_eq!(fates, [true; 3], "pending writes: lost, torn, retained");
    assert_eq!(failed_fates, [true; 3], "failed sync: lost, torn, retained");
    assert_eq!(rename_names, [true; 2], "both atomic rename outcomes");
    assert_eq!(directory_names, [true; 2], "both unsynced directory outcomes");
    assert!(holes, "later sectors can survive without an earlier sector");
    assert!(reordered, "overlapping unsynced writes can reorder");
}

#[test]
fn a_group_spanning_segments_is_reserved_before_any_prepare() {
    for (case, size, count, bytes, segments, behavior, durable_lsn) in [
        ("written rolls", 200, 3, 8, 3, Behavior::Pass, 3),
        ("unused small rolls", 200, 3, 8, 3, Behavior::Dedup, 1),
        ("unused large roll", 8 * 1024 * 1024, 3, 3 * 1024 * 1024, 2, Behavior::Dedup, 1),
        ("unused large rolls", 8 * 1024 * 1024, 7, 3 * 1024 * 1024, 4, Behavior::Dedup, 1),
    ] {
        let disk = Arc::new(SimDisk::new(5));
        let opts =
            TapeOptions { segment_size: size, free_space_floor: Some(0), ..TapeOptions::default() };
        let (mut core, _) = WriterCore::open_with(
            Machine {
                expected_segments: Some((disk.clone(), segments)),
                behavior,
                ..Machine::default()
            },
            disk.clone(),
            Arc::new(SimClock::default()),
            "db",
            opts.clone(),
        )
        .unwrap();
        let replies: Vec<_> = (0..count)
            .map(|_| {
                submit(&mut core, vec![record(Kind::Turn, 1, vec![7; bytes])], Durability::Durable)
            })
            .collect();
        core.step();
        for reply in replies {
            reply.recv().unwrap().unwrap();
        }
        assert_eq!(core.durable_lsn(), durable_lsn, "{case}");
        core.shutdown().unwrap();
        drop(core);
        for reopen in 0..3 {
            let (mut core, report) = WriterCore::open_with(
                Machine::default(),
                disk.clone(),
                Arc::new(SimClock::default()),
                "db",
                opts.clone(),
            )
            .unwrap();
            let expected_lsn = durable_lsn + u64::from(reopen == 2);
            assert_eq!(report.durable_lsn, expected_lsn, "{case}, reopen={reopen}");
            assert_eq!(report.truncated, None, "{case}, reopen={reopen}");
            assert_eq!(report.dropped_bytes, 0, "{case}, reopen={reopen}");
            for lsn in 1..=expected_lsn {
                assert!(core.read(lsn).unwrap().is_some(), "{case}, reopen={reopen}, lsn={lsn}");
            }
            if reopen == 1 {
                let reply =
                    submit(&mut core, vec![record(Kind::Turn, 1, [9; 8])], Durability::Durable);
                core.step();
                let Committed::Durable { last_lsn, .. } = reply.recv().unwrap().unwrap();
                assert_eq!(last_lsn, durable_lsn + 1, "{case}: reservations consumed no LSNs");
            }
            core.shutdown().unwrap();
        }
    }
}

#[test]
fn rewrites_cannot_exceed_the_submitted_space_budget() {
    for (case, segment_size, free, floor, submitted, rewritten, count, sparse, on_prepare) in [
        ("sparse floor growth", 8192, 8192, 4096, 120, 6112, 1, true, None),
        ("one byte growth", 8192, 8192, 4096, 120, 121, 1, true, None),
        ("growth across reserved segments", 256, 8192, 0, 120, 192, 3, false, Some(3)),
        ("same size", 256, 8192, 0, 120, 120, 3, false, None),
        ("shrinking", 256, 8192, 0, 192, 120, 3, false, None),
    ] {
        eprintln!("case: {case}");
        let io = Arc::new(ObservedIo { sparse_allocation: sparse, ..ObservedIo::new(6) });
        let opts =
            TapeOptions { segment_size, free_space_floor: Some(floor), ..TapeOptions::default() };
        let overhead = 2 * HEADER_LEN + 32;
        let machine = Machine::new(Behavior::Resize { len: rewritten - overhead, on_prepare });
        let log = machine.log.clone();
        let (mut core, _) = WriterCore::open_with(
            machine,
            io.clone(),
            Arc::new(SimClock::default()),
            "db",
            opts.clone(),
        )
        .unwrap();
        let dir = Path::new("db");
        let used = io.disk.total_bytes(dir).unwrap() - io.disk.available_bytes(dir).unwrap();
        io.disk.set_capacity(used + free).unwrap();
        assert_eq!(io.disk.available_bytes(dir).unwrap(), free, "{case}: free before prepare");
        let bytes_before = io.segment_bytes.load(Ordering::SeqCst);
        let replies: Vec<_> = (0..count)
            .map(|_| {
                submit(
                    &mut core,
                    vec![record(Kind::Turn, 1, vec![7; submitted - overhead])],
                    Durability::Durable,
                )
            })
            .collect();
        core.step();
        let available = io.disk.available_bytes(dir).unwrap();
        assert!(available >= floor, "{case}: free={available} consumed protected floor={floor}");
        let growth = rewritten > submitted;
        let durable = if growth { on_prepare.unwrap_or(1) - 1 } else { count };
        let durable_size = if growth { submitted } else { rewritten };
        for (index, reply) in replies.into_iter().enumerate() {
            let result = reply.recv().unwrap();
            if index < durable {
                let Committed::Durable { txn, first_lsn, last_lsn, .. } = result.unwrap();
                assert_eq!(txn, index as u64 + 1, "{case}: request={index}");
                assert_eq!(first_lsn, txn, "{case}: request={index}");
                assert_eq!(last_lsn, txn, "{case}: request={index}");
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.outcome, Outcome::NotCommitted, "{case}: request={index}");
                if index == durable {
                    let Error::InvalidFrame(message) = error.kind else {
                        panic!("{case}: {error:?}");
                    };
                    assert_eq!(
                        message,
                        format!(
                            "prepared batch grew from {submitted} to {rewritten} bytes (1 records)"
                        ),
                        "{case}: growth diagnostic"
                    );
                } else {
                    assert!(matches!(error.kind, Error::Poisoned), "{case}: {error:?}");
                }
            }
        }
        assert_eq!(core.is_poisoned(), growth, "{case}");
        assert_eq!(core.durable_lsn(), durable as u64, "{case}");
        let records = core.scan(0).collect::<zerodb_tape::Result<Vec<_>>>().unwrap();
        assert_eq!(records.len(), durable, "{case}");
        assert_eq!(log.lock().unwrap().published, records, "{case}");
        assert!(
            records.iter().all(|r| r.payload.as_ref() == vec![7; durable_size - overhead]),
            "{case}: durable content"
        );
        let prepares = if growth { durable + 1 } else { count };
        assert_eq!(log.lock().unwrap().prepares, prepares, "{case}");
        let bytes_after = bytes_before + (durable * durable_size) as u64;
        assert_eq!(
            io.segment_bytes.load(Ordering::SeqCst),
            bytes_after,
            "{case}: only durable writes"
        );
        if growth {
            if sparse {
                assert_eq!(available, free, "{case}: invalid growth consumed sparse blocks");
            }
            assert!(
                !io.disk.exists(Path::new("db/seg-0000000000000003.zdb")).unwrap(),
                "{case}: no allocation beyond the preflight reservation"
            );
            let next = submit(&mut core, vec![record(Kind::Turn, 1, [7; 8])], Durability::Durable);
            core.step();
            assert!(matches!(next.recv().unwrap().unwrap_err().kind, Error::Poisoned), "{case}");
            assert_eq!(
                log.lock().unwrap().prepares,
                prepares,
                "{case}: later append did not prepare"
            );
            assert_eq!(
                io.segment_bytes.load(Ordering::SeqCst),
                bytes_after,
                "{case}: no later write"
            );
        }
        drop(core);
        let (reopened, report) = WriterCore::open(
            Arc::new(io.disk.crash().unwrap()),
            Arc::new(SimClock::default()),
            "db",
            opts,
        )
        .unwrap();
        assert_eq!(
            report.durable_lsn, durable as u64,
            "{case}: acknowledged prefix survives reopen"
        );
        assert_eq!(
            reopened.scan(0).collect::<zerodb_tape::Result<Vec<_>>>().unwrap(),
            records,
            "{case}: only acknowledged records survive reopen"
        );
    }
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

mod segments {
    use std::path::Path;
    use std::sync::Arc;
    use zerodb_tape::sim::{make_spare, DataDir, SegmentWriter};
    use zerodb_tape::Error;
    use zerodb_tape::{Io, SimDisk};

    #[test]
    fn roll_prepares_names_and_syncs_every_dirty_file() {
        for (case, spare) in [("preallocated roll", false), ("spare roll", true)] {
            for seed in 0..100 {
                let io = Arc::new(SimDisk::new(seed));
                let dir = DataDir::open(io.clone(), "db", None, 512).unwrap();
                let mut writer = SegmentWriter::new(dir, 512, 0).unwrap();
                if spare {
                    let used =
                        SimDisk::DEFAULT_CAPACITY - io.available_bytes("db".as_ref()).unwrap();
                    for (room, made) in [(2 * 512 - 1, false), (2 * 512, true)] {
                        io.set_capacity(used + 100 + room).unwrap();
                        make_spare(io.clone(), Path::new("db"), 512, 100).unwrap();
                        assert_eq!(io.exists("db/spare.zdb".as_ref()).unwrap(), made, "{case}");
                    }
                    io.set_capacity(SimDisk::DEFAULT_CAPACITY).unwrap();
                }
                assert_eq!(writer.write_batch(&[7; 300]).unwrap(), (1, 0));
                assert_eq!(writer.write_batch(&[8; 300]).unwrap(), (2, 0));
                assert!(!io.exists(Path::new("db/spare.zdb")).unwrap(), "{case}: spare taken");
                writer.sync().unwrap();
                drop(writer);
                let crash = io.crash().unwrap();
                let dir = DataDir::open(Arc::new(crash.clone()), "db", None, 512).unwrap();
                assert_eq!(dir.segments(), &[1, 2], "{case}");
                for (id, byte) in [(1, 7), (2, 8)] {
                    eprintln!("case: {case}, id={id:?}, byte={byte:?}");
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
