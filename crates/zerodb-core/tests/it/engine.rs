use crate::support;
use crate::support::sessions::*;
use crate::support::TestDir;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;
use zerodb_core::{Engine, EngineOptions, ErrorKind, Health, Outcome, WindowQuery};
use zerodb_tape::sim::SimClock;
use zerodb_tape::{Io, Kind, SimDisk, Tape};

#[test]
fn windows_are_dense_and_read_cost_depends_only_on_returned_turns() {
    let disk = Arc::new(SimDisk::new(1));
    let engine = open(disk, options());
    let scope = scope("s");
    let mut written = 0;
    for boundary in [1, 16, 17, 5000] {
        eprintln!("case: boundary={boundary:?}");
        while written < boundary {
            let count = (boundary - written).min(128);
            let receipt = engine
                .append(&scope, (written..written + count).map(turn).collect(), None)
                .unwrap();
            assert_eq!(receipt.seqs, (written + 1..=written + count).collect::<Vec<_>>());
            written += count;
        }
        for n in [0, 1, 7, 32] {
            let before = engine.metrics().unwrap().window_reads;
            let window = engine.window(&scope, WindowQuery::Last(n)).unwrap();
            assert_eq!(window.len(), n.min(boundary as usize));
            assert_eq!(engine.metrics().unwrap().window_reads - before, window.len() as u64);
            for pair in window.windows(2) {
                assert_eq!(pair[1].seq, pair[0].seq + 1);
            }
            for value in window {
                assert_eq!(value.content, (value.seq - 1).to_le_bytes());
            }
        }
        let before = engine.metrics().unwrap().window_reads;
        let window = engine.window(&scope, WindowQuery::Since { seq: 1, limit: 3 }).unwrap();
        assert_eq!(window.len(), boundary.min(3) as usize);
        assert_eq!(engine.metrics().unwrap().window_reads - before, window.len() as u64);
        assert_eq!(
            engine.window(&scope, WindowQuery::Since { seq: boundary + 1, limit: 10 }).unwrap(),
            vec![]
        );
    }
}

#[test]
fn retries_conflicts_scope_isolation_and_replay() {
    let disk = Arc::new(SimDisk::new(2));
    let engine = open(disk.clone(), options());
    let s = scope("s");
    let first = engine.append(&s, vec![turn(1), turn(2)], Some("key")).unwrap();
    let durable = engine.metrics().unwrap().durable_lsn;
    assert_eq!(engine.append(&s, vec![turn(1), turn(2)], Some("key")).unwrap(), first);
    assert_eq!(engine.metrics().unwrap().durable_lsn, durable);
    let error = engine.append(&s, vec![turn(9)], Some("key")).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::IdempotencyConflict));
    assert_eq!(error.outcome, Outcome::NotCommitted);
    let mut other = s.clone();
    other.agent = "another-agent".into();
    assert!(matches!(
        engine.append(&other, vec![turn(1), turn(2)], Some("key")).unwrap_err().kind,
        ErrorKind::IdempotencyConflict
    ));
    engine.append(&scope("other"), vec![turn(9)], Some("key")).unwrap();
    other.tenant = "another-tenant".into();
    engine.append(&other, vec![turn(9)], Some("key")).unwrap();
    drop(engine);
    let engine = open(disk.clone(), options());
    assert_eq!(engine.append(&s, vec![turn(1), turn(2)], Some("key")).unwrap(), first);
    engine.checkpoint().unwrap();
    drop(engine);
    let engine = open(disk, options());
    assert_eq!(engine.metrics().unwrap().replayed_records, 0);
    assert_eq!(engine.append(&s, vec![turn(1), turn(2)], Some("key")).unwrap(), first);
}

#[test]
fn tenant_ids_survive_reopen_and_limit_is_typed() {
    for (case, limit) in [("configured tenant limit", 2)] {
        eprintln!("case: {case}");
        let disk = Arc::new(SimDisk::new(3));
        let opts = EngineOptions { max_tenants: limit, ..options() };
        let engine = open(disk.clone(), opts.clone());
        let mut scopes = Vec::new();
        for id in 1..=limit {
            let mut tenant = scope("s");
            if id > 1 {
                tenant.tenant = format!("tenant-{id}");
            }
            let receipt = engine.append(&tenant, vec![turn(id as u64)], None).unwrap();
            assert_eq!(receipt.seqs, vec![1], "{case}, tenant={id}");
            scopes.push(tenant);
        }
        for (index, tenant) in scopes.iter().enumerate() {
            let window = engine.window(tenant, WindowQuery::Last(1)).unwrap();
            assert_eq!(
                window[0].content,
                (index as u64 + 1).to_le_bytes(),
                "{case}, tenant={index}"
            );
        }
        let mut overflow = scope("s");
        overflow.tenant = "overflow".into();
        let error = engine.append(&overflow, vec![turn(1)], None).unwrap_err();
        assert!(matches!(error.kind, ErrorKind::ResourceExhausted("tenants")), "{case}");
        assert_eq!(error.outcome, Outcome::NotCommitted, "{case}");
        drop(engine);
        let (tape, _) =
            Tape::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.tape.clone())
                .unwrap();
        let creates: Vec<_> = tape
            .scan(0)
            .map(|record| record.unwrap())
            .filter(|record| record.kind == Kind::TenantCreate)
            .collect();
        assert_eq!(creates.len(), limit, "{case}");
        for (index, record) in creates.iter().enumerate() {
            assert_eq!(record.tenant_id as usize, index + 1, "{case}, tenant={index}");
        }
        assert_eq!(creates[0].payload.as_ref(), b"tenant", "{case}");
        drop(tape);
        let engine = open(disk.clone(), opts.clone());
        engine.append(&scope("s"), vec![turn(2)], None).unwrap();
        assert_eq!(engine.window(&scope("s"), WindowQuery::Last(2)).unwrap().len(), 2, "{case}");
        assert_eq!(engine.metrics().unwrap().tenants, limit, "{case}");
    }
}

#[test]
fn dedup_scope_bounds_and_oldest_eviction_survive_the_public_api() {
    let disk = Arc::new(SimDisk::new(40));
    let opts = EngineOptions { dedup_per_tenant: 2, dedup_global: 3, ..options() };
    let engine = open(disk.clone(), opts.clone());
    enum Write {
        New,
        Cached,
        ProbeEviction,
    }
    for (case, tenant, session, key, expected_seq, expected_entries, window, write) in [
        ("first tenant", "first", "s", "k", 1, 1, None, Write::New),
        ("session scope", "first", "other", "k", 1, 2, None, Write::New),
        ("tenant scope", "second", "s", "k", 1, 3, Some(1), Write::New),
        ("per-tenant oldest eviction", "first", "s", "new", 2, 3, Some(1), Write::ProbeEviction),
        ("global oldest eviction", "third", "s", "k", 1, 3, Some(1), Write::New),
        ("other tenant key survives", "second", "s", "k", 1, 3, Some(1), Write::Cached),
        ("oldest session key was evicted", "first", "other", "k", 2, 3, Some(1), Write::New),
    ] {
        eprintln!("case: {case}");
        let mut scope = scope(session);
        scope.tenant = tenant.into();
        let before = engine.metrics().unwrap().durable_lsn;
        let receipt = engine.append(&scope, vec![turn(1)], Some(key)).unwrap();
        assert_eq!(receipt.seqs, vec![expected_seq], "{case}");
        assert_eq!(engine.metrics().unwrap().dedup_entries, expected_entries, "{case}");
        if matches!(write, Write::Cached) {
            assert_eq!(engine.metrics().unwrap().durable_lsn, before, "{case}");
        } else {
            assert!(engine.metrics().unwrap().durable_lsn > before, "{case}");
        }
        if let Some(window) = window {
            assert_eq!(engine.metrics().unwrap().dedup_window_effective, window, "{case}");
        }
        if matches!(write, Write::ProbeEviction) {
            engine.checkpoint().unwrap();
            let image = Arc::new(disk.crash().unwrap());
            let fork = open(image, opts.clone());
            let retry = fork.append(&scope, vec![turn(1)], Some("k")).unwrap();
            assert_eq!(retry.seqs, vec![3], "{case}: the evicted key must write again");
            assert!(fork.metrics().unwrap().durable_lsn > receipt.last_lsn, "{case}");
        }
    }

    let mut checkpoint_scope = scope("checkpoint");
    checkpoint_scope.tenant = "first".into();
    for (case, range) in [("first checkpoint", 0..3), ("second checkpoint", 3..6)] {
        eprintln!("case: {case}");
        for i in range {
            engine.append(&checkpoint_scope, vec![turn(i)], Some(&i.to_string())).unwrap();
        }
        assert_eq!(engine.metrics().unwrap().dedup_entries, 3, "{case}");
        engine.checkpoint().unwrap();
    }
    drop(engine);
    let engine = open(disk, opts);
    assert_eq!(engine.metrics().unwrap().replayed_records, 0);
    assert_eq!(engine.metrics().unwrap().dedup_entries, 3);
    assert_eq!(engine.metrics().unwrap().dedup_window_effective, 1);
    assert_eq!(engine.append(&checkpoint_scope, vec![turn(5)], Some("5")).unwrap().seqs, vec![6]);
    assert_eq!(engine.append(&checkpoint_scope, vec![turn(0)], Some("0")).unwrap().seqs, vec![7]);
}

#[test]
fn checkpoint_bounds_replay_and_corrupt_derived_data_falls_back() {
    let disk = Arc::new(SimDisk::new(5));
    let engine = open(disk.clone(), options());
    engine.append(&scope("s"), (0..100).map(turn).collect(), Some("initial")).unwrap();
    engine.checkpoint().unwrap();
    engine.append(&scope("s"), (100..107).map(turn).collect(), Some("tail")).unwrap();
    let oracle = engine.window(&scope("s"), WindowQuery::Last(200)).unwrap();
    drop(engine);
    let engine = open(disk.clone(), options());
    assert_eq!(engine.metrics().unwrap().replayed_records, 7);
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(200)).unwrap(), oracle);
    drop(engine);
    let checkpoint = disk
        .list(std::path::Path::new("db"))
        .unwrap()
        .into_iter()
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("ckpt-"))
        .unwrap();
    let file = disk.open(&checkpoint).unwrap();
    disk.flip_bit(file, 12).unwrap();
    disk.close(file).unwrap();
    let engine = open(disk, options());
    assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 1);
    assert_eq!(engine.metrics().unwrap().replayed_records, 108);
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(200)).unwrap(), oracle);
}

#[test]
fn hot_page_budget_evicts_and_reloads_cold_sessions() {
    let disk = Arc::new(SimDisk::new(6));
    let opts = EngineOptions { hot_page_bytes: 128, ..options() };
    let engine = open(disk.clone(), opts.clone());
    for i in 0u64..8 {
        let s = scope(&i.to_string());
        engine.append(&s, vec![turn(i)], Some("key")).unwrap();
        engine.checkpoint().unwrap();
        assert!(engine.metrics().unwrap().hot_page_bytes <= 128);
    }
    for i in 0u64..8 {
        let s = scope(&i.to_string());
        assert_eq!(engine.window(&s, WindowQuery::Last(1)).unwrap()[0].content, i.to_le_bytes());
    }
    engine.append(&scope("0"), vec![turn(8)], None).unwrap();
    engine.checkpoint().unwrap();
    drop(engine);
    let engine = open(disk, opts);
    assert_eq!(engine.window(&scope("0"), WindowQuery::Last(2)).unwrap().len(), 2);
    assert!(engine.metrics().unwrap().hot_page_bytes <= 128);
}

#[test]
fn write_outages_report_health_preserve_reads_and_retry_keys() {
    #[derive(Clone, Copy)]
    enum Fault {
        Floor,
        ColdFloor,
        ReopenFull,
        ReopenDirtyFull,
        ReopenAllocation,
        ReopenStat,
        SpaceQuery,
        Sync,
    }
    for (case, fault, health) in [
        ("disk floor", Fault::Floor, Health::ReadOnly),
        ("cold reads with no free space", Fault::ColdFloor, Health::ReadOnly),
        ("reopen while full", Fault::ReopenFull, Health::ReadOnly),
        ("reopen with dirty pages then full", Fault::ReopenDirtyFull, Health::ReadOnly),
        ("reopen allocation race", Fault::ReopenAllocation, Health::ReadOnly),
        ("reopen with space query failure", Fault::ReopenStat, Health::ReadOnly),
        ("space query outage", Fault::SpaceQuery, Health::ReadOnly),
        ("ambiguous sync", Fault::Sync, Health::Poisoned),
    ] {
        let seeds = if matches!(fault, Fault::Sync) { 32 } else { 1 };
        for seed in 0..seeds {
            eprintln!("case: {case}, seed={seed}");
            let io = Arc::new(support::ProbeIo::new(seed));
            let mut opts = options();
            opts.tape.free_space_floor = Some(4096);
            let history = if matches!(
                fault,
                Fault::ColdFloor
                    | Fault::ReopenFull
                    | Fault::ReopenDirtyFull
                    | Fault::ReopenAllocation
                    | Fault::ReopenStat
            ) {
                512
            } else {
                1
            };
            if matches!(fault, Fault::ColdFloor | Fault::ReopenDirtyFull) {
                opts.tape.lsn_index_bytes = 4224;
            }
            let mut engine =
                Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", opts.clone())
                    .unwrap();
            assert_eq!(engine.health(), Health::Ok, "{case}");
            let s = scope("s");
            let original = engine.append(&s, vec![turn(1); history], Some("original")).unwrap();
            let before = engine.metrics().unwrap();
            match fault {
                Fault::Floor | Fault::ColdFloor => {
                    let capacity = io.disk.total_bytes(std::path::Path::new("db")).unwrap();
                    let free = io.disk.available_bytes(std::path::Path::new("db")).unwrap();
                    let remaining = if matches!(fault, Fault::ColdFloor) { 0 } else { 4095 };
                    io.disk.set_capacity(capacity - free + remaining).unwrap();
                }
                Fault::ReopenFull
                | Fault::ReopenDirtyFull
                | Fault::ReopenAllocation
                | Fault::ReopenStat => {
                    if matches!(fault, Fault::ReopenDirtyFull) {
                        // Skip open-time replay, so recovery's last dirty page stays resident.
                        engine.checkpoint().unwrap();
                    }
                    drop(engine);
                    for name in ["LSN-OFFSETS", "LSN-ROUTES"] {
                        let file = io.disk.open(&std::path::Path::new("db").join(name)).unwrap();
                        io.disk.set_len(file, 0).unwrap();
                        io.disk.set_len(file, 4096).unwrap();
                        io.disk.sync_data(file).unwrap();
                        io.disk.close(file).unwrap();
                    }
                    if matches!(fault, Fault::ReopenFull | Fault::ReopenDirtyFull) {
                        let capacity = io.disk.total_bytes(std::path::Path::new("db")).unwrap();
                        let free = io.disk.available_bytes(std::path::Path::new("db")).unwrap();
                        // Recovery write-backs fit in 5 pages. Reservation plus floor needs 6.
                        let headroom =
                            if matches!(fault, Fault::ReopenDirtyFull) { 20480 } else { 0 };
                        io.disk.set_capacity(capacity - free + headroom).unwrap();
                    } else if matches!(fault, Fault::ReopenAllocation) {
                        io.allocation_full.store(true, Ordering::SeqCst);
                    } else {
                        opts.tape.free_space_floor = None;
                        io.space_error.store(true, Ordering::SeqCst);
                    }
                    engine = Engine::open_with(
                        io.clone(),
                        Arc::new(SimClock::default()),
                        "db",
                        opts.clone(),
                    )
                    .unwrap();
                    assert_eq!(engine.health(), Health::ReadOnly, "{case}: open did not poison");
                    if matches!(fault, Fault::ReopenAllocation) {
                        assert!(
                            !io.allocation_full.load(Ordering::SeqCst),
                            "{case}: reservation was attempted"
                        );
                    }
                    if matches!(fault, Fault::ReopenAllocation | Fault::ReopenDirtyFull) {
                        let capacity = io.disk.total_bytes(std::path::Path::new("db")).unwrap();
                        let free = io.disk.available_bytes(std::path::Path::new("db")).unwrap();
                        io.disk.set_capacity(capacity - free).unwrap();
                    }
                    let full_before = io.disk.storage_full_returns().unwrap();
                    let oldest =
                        engine.window(&s, WindowQuery::Since { seq: 1, limit: 1 }).unwrap();
                    assert_eq!(
                        oldest[0].content,
                        turn(1).content,
                        "{case}: acknowledged turn survived reopen"
                    );
                    assert_eq!(oldest[0].lsn, original.first_lsn, "{case}");
                    if matches!(fault, Fault::ReopenDirtyFull) {
                        let index = engine.metrics().unwrap().tape_index;
                        assert!(
                            io.disk.storage_full_returns().unwrap() > full_before,
                            "{case}: cold read could not write back a dirty page"
                        );
                        assert!(
                            index.resident_bytes <= index.budget_bytes + 4224,
                            "{case}: cold read overshoot is one page"
                        );
                    }
                    if matches!(
                        fault,
                        Fault::ReopenFull | Fault::ReopenDirtyFull | Fault::ReopenAllocation
                    ) {
                        assert_eq!(
                            io.disk.available_bytes(std::path::Path::new("db")).unwrap(),
                            0,
                            "{case}: zero free after reopen"
                        );
                    }
                }
                Fault::SpaceQuery => io.space_error.store(true, Ordering::SeqCst),
                Fault::Sync => io.disk.fail_next_sync().unwrap(),
            }
            for attempt in 0..2 {
                let error = engine.append(&s, vec![turn(2)], Some("retry")).unwrap_err();
                let expected = if matches!(fault, Fault::Sync) && attempt == 0 {
                    Outcome::Unknown
                } else {
                    Outcome::NotCommitted
                };
                assert_eq!(error.outcome, expected, "{case}, attempt={attempt}");
                assert!(
                    match fault {
                        Fault::Floor
                        | Fault::ColdFloor
                        | Fault::ReopenFull
                        | Fault::ReopenDirtyFull
                        | Fault::ReopenAllocation =>
                            matches!(error.kind, ErrorKind::Storage(zerodb_tape::Error::DiskFull)),
                        Fault::SpaceQuery | Fault::ReopenStat => matches!(
                            error.kind,
                            ErrorKind::Storage(zerodb_tape::Error::Io(
                                std::io::ErrorKind::Other,
                                ref message
                            )) if message == "injected space query failure"
                        ),
                        Fault::Sync if attempt == 0 => matches!(
                            error.kind,
                            ErrorKind::Storage(zerodb_tape::Error::SyncFailed(_))
                        ),
                        Fault::Sync => matches!(error.kind, ErrorKind::Poisoned),
                    },
                    "{case}, attempt={attempt}: {error:?}"
                );
                assert_eq!(engine.health(), health, "{case}");
                if !matches!(fault, Fault::Sync) {
                    assert_eq!(
                        engine.append(&s, vec![turn(1); history], Some("original")).unwrap(),
                        original,
                        "{case}: committed retries keep their durable receipt"
                    );
                    let conflict = engine.append(&s, vec![turn(2)], Some("original")).unwrap_err();
                    assert!(matches!(conflict.kind, ErrorKind::IdempotencyConflict), "{case}");
                    assert_eq!(conflict.outcome, Outcome::NotCommitted, "{case}");
                    assert_eq!(engine.health(), health, "{case}: retries do not recheck space");
                }
                if matches!(fault, Fault::ColdFloor) {
                    assert!(
                        before.tape_index.dirty_page_evictions > 0,
                        "{case}: multiple dirty pages"
                    );
                    let before_cold = engine.metrics().unwrap().tape_index;
                    let cold = engine.window(&s, WindowQuery::Since { seq: 1, limit: 3 }).unwrap();
                    let after_cold = engine.metrics().unwrap().tape_index;
                    if attempt == 0 {
                        assert!(
                            after_cold.dirty_page_evictions > before_cold.dirty_page_evictions,
                            "{case}: oldest read evicted a dirty page"
                        );
                    }
                    assert_eq!(
                        cold.iter().map(|turn| turn.seq).collect::<Vec<_>>(),
                        vec![1, 2, 3],
                        "{case}"
                    );
                    assert!(cold.iter().all(|value| value.content == turn(1).content), "{case}");
                    assert!(
                        after_cold.page_reloads > before_cold.page_reloads,
                        "{case}: cold index read"
                    );
                    assert_eq!(
                        io.disk.available_bytes(std::path::Path::new("db")).unwrap(),
                        0,
                        "{case}"
                    );
                    assert_eq!(
                        engine.health(),
                        Health::ReadOnly,
                        "{case}: cold read kept read-only"
                    );
                }
                let window = engine.window(&s, WindowQuery::Last(10)).unwrap();
                assert_eq!(window.len(), history.min(10), "{case}");
                assert!(window.iter().all(|value| value.content == turn(1).content), "{case}");
                assert_eq!(window.last().unwrap().lsn, original.last_lsn, "{case}");
                let after = engine.metrics().unwrap();
                assert_eq!(after.durable_lsn, before.durable_lsn, "{case}");
                assert_eq!(after.dedup_entries, before.dedup_entries, "{case}");
                assert_eq!(after.tenants, before.tenants, "{case}");
            }
            io.disk.set_capacity(1 << 40).unwrap();
            io.space_error.store(false, Ordering::SeqCst);
            assert_eq!(engine.health(), health, "{case}: no write has rechecked space");
            if matches!(fault, Fault::Sync) {
                drop(engine);
                engine = open(Arc::new(io.disk.crash().unwrap()), opts.clone());
                assert_eq!(engine.health(), Health::Ok, "{case}");
            }
            let retried = engine.append(&s, vec![turn(2)], Some("retry")).unwrap();
            assert_eq!(engine.health(), Health::Ok, "{case}");
            if matches!(
                fault,
                Fault::ReopenFull
                    | Fault::ReopenDirtyFull
                    | Fault::ReopenAllocation
                    | Fault::ReopenStat
            ) {
                for (name, width) in [("LSN-OFFSETS", 8), ("LSN-ROUTES", 16)] {
                    let file = io.disk.open(&std::path::Path::new("db").join(name)).unwrap();
                    let required = (retried.last_lsn * width).div_ceil(4096) * 4096;
                    assert_eq!(
                        io.disk.len(file).unwrap(),
                        required,
                        "{case}: retried {name} reservation"
                    );
                    io.disk.close(file).unwrap();
                }
            }
            assert_eq!(retried.seqs, vec![history as u64 + 1], "{case}");
            assert_eq!(retried.txn, original.txn + 1, "{case}");
            assert_eq!(retried.first_lsn, original.last_lsn + 1, "{case}");
            assert_eq!(retried.last_lsn, retried.first_lsn, "{case}");
            assert_eq!(engine.append(&s, vec![turn(2)], Some("retry")).unwrap(), retried, "{case}");
            assert_eq!(engine.metrics().unwrap().durable_lsn, retried.last_lsn, "{case}");
            assert_eq!(engine.metrics().unwrap().dedup_entries, before.dedup_entries + 1, "{case}");
            let window = engine.window(&s, WindowQuery::Last(10)).unwrap();
            assert_eq!(
                window.iter().map(|turn| turn.seq).collect::<Vec<_>>(),
                (history as u64 + 1 - history.min(9) as u64..=history as u64 + 1)
                    .collect::<Vec<_>>(),
                "{case}"
            );
            assert!(
                window[..window.len() - 1].iter().all(|value| value.content == turn(1).content),
                "{case}"
            );
            assert_eq!(window.last().unwrap().content, turn(2).content, "{case}");
            assert_eq!(
                engine.append(&s, vec![turn(1); history], Some("original")).unwrap(),
                original,
                "{case}"
            );
            if !matches!(fault, Fault::Sync) {
                drop(engine);
                let engine = open(Arc::new(io.disk.crash().unwrap()), opts);
                assert_eq!(engine.health(), Health::Ok, "{case}");
                assert_eq!(
                    engine.append(&s, vec![turn(1); history], Some("original")).unwrap(),
                    original,
                    "{case}"
                );
                assert_eq!(
                    engine.append(&s, vec![turn(2)], Some("retry")).unwrap(),
                    retried,
                    "{case}"
                );
                assert_eq!(engine.window(&s, WindowQuery::Last(10)).unwrap(), window, "{case}");
            }
        }
    }
}

#[test]
fn concurrent_same_key_writes_once() {
    let engine = open(Arc::new(SimDisk::new(8)), options());
    let barrier = Barrier::new(8);
    std::thread::scope(|threads| {
        let mut handles = Vec::new();
        for _ in 0..8 {
            handles.push(threads.spawn(|| {
                barrier.wait();
                engine.append(&scope("s"), vec![turn(1), turn(2)], Some("same")).unwrap()
            }));
        }
        let receipts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(receipts.iter().all(|r| r == &receipts[0]));
    });
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(100)).unwrap().len(), 2);
    assert_eq!(engine.metrics().unwrap().durable_lsn, 3);
}

#[test]
fn invalid_batches_leave_no_sequences_or_tenants() {
    let engine = open(Arc::new(SimDisk::new(9)), options());
    assert!(engine.append(&scope("s"), vec![], None).is_err());
    assert!(engine.append(&scope("s"), vec![turn(1); 1024], None).is_err());
    let mut too_big = turn(1);
    too_big.content = vec![0; 4 * 1024 * 1024];
    assert!(engine.append(&scope("s"), vec![too_big], None).is_err());
    assert_eq!(engine.metrics().unwrap().tenants, 0);
    assert_eq!(engine.append(&scope("s"), vec![turn(1)], None).unwrap().seqs, vec![1]);
}

#[test]
fn session_and_page_directories_have_typed_bounds() {
    let opts = EngineOptions { max_sessions: 1, max_pages: 1, ..options() };
    let engine = open(Arc::new(SimDisk::new(10)), opts);
    engine.append(&scope("s"), vec![turn(1); 16], None).unwrap();
    assert!(matches!(
        engine.append(&scope("other"), vec![turn(1)], None).unwrap_err().kind,
        ErrorKind::ResourceExhausted("sessions")
    ));
    assert!(matches!(
        engine.append(&scope("s"), vec![turn(1)], None).unwrap_err().kind,
        ErrorKind::ResourceExhausted("page_directory")
    ));
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(100)).unwrap().len(), 16);
}

#[test]
fn automatic_worker_checkpoint_and_checkpoint_failure_do_not_poison_tape() {
    let disk = Arc::new(SimDisk::new(11));
    let opts = EngineOptions { checkpoint_bytes: 1, ..options() };
    let engine = open(disk, opts);
    let receipt = engine.append(&scope("s"), vec![turn(1)], None).unwrap();
    for _ in 0..1000 {
        if engine.metrics().unwrap().checkpoint_watermark == receipt.last_lsn {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(engine.metrics().unwrap().checkpoint_watermark, receipt.last_lsn);
    drop(engine);
    let disk = Arc::new(SimDisk::new(12));
    let engine = open(disk.clone(), options());
    engine.append(&scope("s"), vec![turn(1)], None).unwrap();
    disk.fail_next_sync().unwrap();
    assert!(engine.checkpoint().is_err());
    assert_eq!(engine.health(), Health::Ok);
    engine.append(&scope("s"), vec![turn(2)], None).unwrap();
    assert_eq!(engine.health(), Health::Ok);
    engine.checkpoint().unwrap();
    drop(engine);
    let engine = open(disk, options());
    assert_eq!(engine.metrics().unwrap().replayed_records, 0);
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(10)).unwrap().len(), 2);
}

#[test]
fn corrupt_checkpoint_full_replay_can_spill_with_a_one_page_budget() {
    let disk = Arc::new(SimDisk::new(13));
    let opts = EngineOptions { hot_page_bytes: 128, ..options() };
    let engine = open(disk.clone(), opts.clone());
    for i in 0u64..12 {
        engine.append(&scope(&i.to_string()), vec![turn(i)], Some("key")).unwrap();
        engine.checkpoint().unwrap();
    }
    drop(engine);
    let file = disk.open(std::path::Path::new("db/CHECKPOINT")).unwrap();
    disk.flip_bit(file, 12).unwrap();
    disk.close(file).unwrap();
    let engine = open(disk, opts);
    for i in 0u64..12 {
        assert_eq!(
            engine.window(&scope(&i.to_string()), WindowQuery::Last(1)).unwrap()[0].content,
            i.to_le_bytes()
        );
        assert_eq!(
            engine.append(&scope(&i.to_string()), vec![turn(i)], Some("key")).unwrap().seqs,
            vec![1]
        );
    }
    assert!(engine.metrics().unwrap().hot_page_bytes <= 128);
}

#[test]
fn long_session_evicts_its_own_old_pages() {
    let engine =
        open(Arc::new(SimDisk::new(14)), EngineOptions { hot_page_bytes: 32768, ..options() });
    let s = scope("long");
    for i in 0..6000 {
        engine.append(&s, vec![turn(i)], None).unwrap();
        if i % 100 == 0 {
            engine.checkpoint().unwrap();
        }
    }
    assert_eq!(engine.window(&s, WindowQuery::Last(3)).unwrap()[0].seq, 5998);
    assert_eq!(engine.window(&s, WindowQuery::Since { seq: 1, limit: 3 }).unwrap()[0].seq, 1);
    assert!(engine.metrics().unwrap().hot_page_bytes <= 32768);
}

#[test]
fn receipts_need_no_reads_and_checkpoint_io_uses_worker() {
    let io = Arc::new(support::ProbeIo::new(15));
    let engine =
        Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", options()).unwrap();
    io.fail_read.store(true, Ordering::SeqCst);
    let receipt = engine.append(&scope("s"), vec![turn(1)], Some("key")).unwrap();
    assert_eq!(receipt.seqs, vec![1]);
    assert_eq!(engine.append(&scope("s"), vec![turn(1)], Some("key")).unwrap(), receipt);
    assert!(io.fail_read.swap(false, Ordering::SeqCst), "append and retry must not read");
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(1)).unwrap()[0].seq, 1);
    assert_eq!(engine.append(&scope("s"), vec![turn(1)], Some("key")).unwrap().seqs, vec![1]);
    engine.checkpoint().unwrap();
    let writes = io.writes.lock().unwrap();
    let checkpoint_writes: Vec<_> = writes
        .iter()
        .filter(|(_, path)| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.starts_with("ckpt-") || name == "CHECKPOINT.next"
        })
        .collect();
    assert!(!checkpoint_writes.is_empty());
    assert!(checkpoint_writes.iter().all(|(thread, _)| thread == "zerodb-checkpoint"));
}

#[test]
fn checkpoint_file_is_written_in_one_buffer() {
    let io = Arc::new(support::ProbeIo::new(42));
    let engine =
        Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", options()).unwrap();
    for i in 0..20 {
        engine.append(&scope(&format!("session-{i}")), vec![turn(i)], None).unwrap();
    }

    io.writes.lock().unwrap().clear();
    engine.checkpoint().unwrap();
    let writes = io.writes.lock().unwrap();
    let data_writes = writes
        .iter()
        .filter(|(_, path)| path.file_name().unwrap().to_str().unwrap().starts_with("ckpt-"))
        .count();
    assert_eq!(data_writes, 1);
}

#[test]
fn periodic_superblock_io_does_not_run_on_the_writer() {
    let io = Arc::new(support::ProbeIo::new(43));
    let clock = Arc::new(SimClock::default());
    let engine = Engine::open_with(io.clone(), clock.clone(), "db", options()).unwrap();
    io.writes.lock().unwrap().clear();
    clock.advance(31_000_000_000);
    engine.append(&scope("s"), vec![turn(1)], None).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let names = io
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.file_name().unwrap() == "SUPERBLOCK.tmp")
            .map(|(thread, _)| thread.clone())
            .collect::<Vec<_>>();
        if !names.is_empty() {
            assert!(names.iter().all(|name| name == "zerodb-superblock"), "{names:?}");
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn replay_spills_by_dirty_capacity_not_clean_resident_pages() {
    let io = Arc::new(support::ProbeIo::new(16));
    let engine =
        Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", options()).unwrap();
    for i in 0..1300 {
        engine.append(&scope(&i.to_string()), vec![turn(i)], None).unwrap();
    }
    drop(engine);
    io.writes.lock().unwrap().clear();
    let opts = EngineOptions { hot_page_bytes: 128 * 1024, ..options() };
    let engine = Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", opts).unwrap();
    let files: std::collections::BTreeSet<_> = io
        .writes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, p)| p.file_name().unwrap().to_str().unwrap().starts_with("ckpt-"))
        .map(|(_, p)| p.clone())
        .collect();
    assert!(!files.is_empty());
    assert!(files.len() <= 3, "{} checkpoints for 1300 dirty pages", files.len());
    assert_eq!(
        engine.window(&scope("1299"), WindowQuery::Last(1)).unwrap()[0].content,
        1299u64.to_le_bytes()
    );
}

#[test]
fn failed_sync_with_a_full_dedup_window_cannot_checkpoint_tentative_eviction() {
    let disk = Arc::new(SimDisk::new(17));
    let opts = EngineOptions { dedup_per_tenant: 1, dedup_global: 1, ..options() };
    let engine = open(disk.clone(), opts.clone());
    let receipt = engine.append(&scope("s"), vec![turn(1)], Some("original")).unwrap();
    let watermark = engine.checkpoint().unwrap();
    disk.fail_next_sync().unwrap();
    assert_eq!(
        engine.append(&scope("s"), vec![turn(2)], Some("unknown")).unwrap_err().outcome,
        Outcome::Unknown
    );
    assert_eq!(engine.checkpoint().unwrap(), watermark);
    drop(engine);
    let engine = open(Arc::new(disk.crash().unwrap()), opts);
    let window = engine.window(&scope("s"), WindowQuery::Last(10)).unwrap();
    if window.len() == 1 {
        assert_eq!(engine.append(&scope("s"), vec![turn(1)], Some("original")).unwrap(), receipt);
    } else {
        assert_eq!(
            engine.append(&scope("s"), vec![turn(2)], Some("unknown")).unwrap().seqs,
            vec![2]
        );
    }
}

#[test]
fn cold_window_and_pin_io_do_not_hold_the_writer_state_lock() {
    for append in [false, true] {
        eprintln!("case: append={append:?}");
        let io = Arc::new(support::ProbeIo::new(18));
        let opts = EngineOptions { hot_page_bytes: 256, ..options() };
        let engine =
            Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", opts).unwrap();
        for i in 0..3 {
            engine.append(&scope(&i.to_string()), vec![turn(i)], None).unwrap();
            engine.checkpoint().unwrap();
        }
        let (entered, blocked) = std::sync::mpsc::sync_channel(1);
        let (release, resumed) = std::sync::mpsc::sync_channel(1);
        *io.read_gate.lock().unwrap() = Some((entered, resumed));
        std::thread::scope(|threads| {
            threads.spawn(|| {
                if append {
                    engine.append(&scope("0"), vec![turn(10)], None).unwrap();
                } else {
                    engine.window(&scope("0"), WindowQuery::Last(1)).unwrap();
                }
            });
            blocked.recv_timeout(Duration::from_secs(2)).unwrap();
            let (done, completed) = std::sync::mpsc::sync_channel(1);
            let engine = &engine;
            threads.spawn(move || {
                let result = engine.append(&scope("2"), vec![turn(11)], None);
                let _ = done.send(result);
            });
            let result = completed.recv_timeout(Duration::from_secs(1));
            if result.is_ok() {
                for i in 0..6 {
                    engine.append(&scope("2"), vec![turn(20 + i)], None).unwrap();
                    engine.checkpoint().unwrap();
                }
            }
            release.send(()).unwrap();
            assert!(result.is_ok(), "writer stalled behind cold I/O, append={append}");
            result.unwrap().unwrap();
        });
    }
}

#[test]
fn open_sweeps_orphan_checkpoints_only_after_taking_the_tape_lock() {
    let disk = Arc::new(SimDisk::new(19));
    let engine = open(disk.clone(), options());
    engine.append(&scope("s"), vec![turn(1)], None).unwrap();
    engine.checkpoint().unwrap();
    for path in ["db/ckpt-orphan.zdb", "db/CHECKPOINT.next"] {
        let id = disk.create(std::path::Path::new(path)).unwrap();
        disk.write_at(id, 0, b"partial").unwrap();
        disk.sync_data(id).unwrap();
        disk.close(id).unwrap();
    }
    disk.sync_dir(std::path::Path::new("db")).unwrap();
    assert!(
        Engine::open_with(disk.clone(), Arc::new(SimClock::default()), "db", options()).is_err()
    );
    assert!(disk.exists(std::path::Path::new("db/ckpt-orphan.zdb")).unwrap());
    drop(engine);
    let engine = open(disk.clone(), options());
    assert!(!disk.exists(std::path::Path::new("db/ckpt-orphan.zdb")).unwrap());
    assert!(!disk.exists(std::path::Path::new("db/CHECKPOINT.next")).unwrap());
    assert_eq!(engine.metrics().unwrap().checkpoint_files, 1);
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(1)).unwrap()[0].seq, 1);
}

#[test]
fn checkpoints_advance_while_tiny_budget_writers_append_continuously() {
    let engine =
        open(Arc::new(SimDisk::new(20)), EngineOptions { hot_page_bytes: 2048, ..options() });
    let barrier = Barrier::new(5);
    let active = std::sync::atomic::AtomicUsize::new(4);
    let acked = AtomicU64::new(0);
    std::thread::scope(|threads| {
        for thread in 0..4 {
            let engine = &engine;
            let barrier = &barrier;
            let active = &active;
            let acked = &acked;
            threads.spawn(move || {
                barrier.wait();
                let result = (|| -> Result<(), zerodb_core::EngineError> {
                    for i in 0..128 {
                        let session = format!("{thread}-{i}");
                        let receipt =
                            engine.append(&scope(&session), vec![turn(i)], Some("key"))?;
                        acked.fetch_max(receipt.last_lsn, Ordering::Release);
                    }
                    Ok(())
                })();
                active.fetch_sub(1, Ordering::Release);
                result.unwrap();
            });
        }
        threads.spawn(|| {
            barrier.wait();
            let mut advances = 0;
            let mut previous = 0;
            while active.load(Ordering::Acquire) > 0 {
                let target = acked.load(Ordering::Acquire);
                let watermark = engine.checkpoint().unwrap();
                assert!(
                    watermark >= target,
                    "checkpoint {watermark} missed acknowledged boundary {target}"
                );
                if watermark > previous {
                    advances += 1;
                    previous = watermark;
                }
                std::thread::sleep(Duration::from_micros(100));
            }
            assert!(advances > 1, "checkpoints did not advance under load");
        });
    });
    assert_eq!(engine.checkpoint().unwrap(), acked.load(Ordering::Acquire));
    for thread in 0..4 {
        for i in 0..128 {
            assert_eq!(
                engine.window(&scope(&format!("{thread}-{i}")), WindowQuery::Last(1)).unwrap()[0]
                    .seq,
                1
            );
        }
    }
}

#[test]
fn stdio_eight_threads_sixty_four_sessions_reopen_equals_oracle() {
    let dir = TestDir::new();
    let engine = Engine::open(&dir.0, options()).unwrap();
    let oracle = Mutex::new(HashMap::<u64, Vec<(u64, Vec<u8>)>>::new());
    let barrier = Barrier::new(8);
    std::thread::scope(|threads| {
        for thread in 0..8 {
            let engine = &engine;
            let oracle = &oracle;
            let barrier = &barrier;
            threads.spawn(move || {
                barrier.wait();
                for round in 0..32 {
                    let session = thread * 8 + round % 8;
                    let value = thread * 1000 + round;
                    let s = scope(&session.to_string());
                    let receipt =
                        engine.append(&s, vec![turn(value)], Some(&round.to_string())).unwrap();
                    let recent = engine.window(&s, WindowQuery::Last(1)).unwrap();
                    assert_eq!(recent[0].seq, receipt.seqs[0]);
                    oracle
                        .lock()
                        .unwrap()
                        .entry(session)
                        .or_default()
                        .push((receipt.seqs[0], value.to_le_bytes().to_vec()));
                }
            });
        }
    });
    engine.checkpoint().unwrap();
    drop(engine);
    let engine = Engine::open(&dir.0, options()).unwrap();
    for (session, mut values) in oracle.into_inner().unwrap() {
        values.sort_by_key(|v| v.0);
        let window = engine.window(&scope(&session.to_string()), WindowQuery::Last(100)).unwrap();
        assert_eq!(window.iter().map(|t| (t.seq, t.content.clone())).collect::<Vec<_>>(), values);
    }
    drop(engine);
}

mod metrics {
    use crate::support::sessions::*;
    use std::sync::Arc;
    use zerodb_core::EngineOptions;
    use zerodb_tape::{SimDisk, TapeOptions};

    #[test]
    fn engine_metrics_expose_eviction_without_changing_the_index_budget() {
        let opts = EngineOptions {
            tape: TapeOptions { segment_size: 8192, lsn_index_bytes: 12 * 1024, ..options().tape },
            ..options()
        };
        let engine = open(Arc::new(SimDisk::new(78)), opts);
        assert_eq!(engine.metrics().unwrap().tape_index.page_evictions, 0);
        let scope = scope("s");
        for _ in 0..20 {
            engine.append(&scope, vec![turn(u64::from_le_bytes([7; 8])); 64], None).unwrap();
        }
        let index = engine.metrics().unwrap().tape_index;
        assert!(index.page_evictions > 0);
        assert!(index.dirty_page_evictions > 0);
        assert_eq!(index.budget_bytes, 12 * 1024);
        assert!(index.resident_bytes <= index.budget_bytes);
    }
}
