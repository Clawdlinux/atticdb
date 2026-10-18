#![allow(clippy::unwrap_used)]
mod support;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;
use zerodb_core::{
    Engine, EngineOptions, ErrorKind, NewTurn, Outcome, Role, Scope, Trust, WindowQuery,
};
use zerodb_tape::sim::SimClock;
use zerodb_tape::{Io, Kind, SimDisk, Tape, TapeOptions};

fn scope(session: &str) -> Scope {
    Scope { tenant: "tenant".into(), agent: "agent".into(), session: session.into() }
}
fn turn(value: u64) -> NewTurn {
    NewTurn {
        role: Role::User,
        content: value.to_le_bytes().to_vec(),
        meta: vec![],
        trust: Trust::Untrusted,
    }
}
fn options() -> EngineOptions {
    EngineOptions {
        tape: TapeOptions { segment_size: 64 * 1024, ..TapeOptions::default() },
        checkpoint_interval: Duration::from_secs(3600),
        checkpoint_bytes: u64::MAX,
        ..EngineOptions::default()
    }
}
fn open(disk: Arc<SimDisk>, opts: EngineOptions) -> Engine {
    Engine::open_with(disk, Arc::new(SimClock::default()), "db", opts).unwrap()
}

#[test]
fn windows_are_dense_and_read_cost_depends_only_on_returned_turns() {
    let disk = Arc::new(SimDisk::new(1));
    let engine = open(disk, options());
    let scope = scope("s");
    let mut written = 0;
    for boundary in [1, 16, 17, 5000] {
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
    let disk = Arc::new(SimDisk::new(3));
    let opts = EngineOptions { max_tenants: 2, ..options() };
    let engine = open(disk.clone(), opts.clone());
    engine.append(&scope("s"), vec![turn(1)], None).unwrap();
    let mut other = scope("s");
    other.tenant = "other".into();
    engine.append(&other, vec![turn(1)], None).unwrap();
    other.tenant = "overflow".into();
    let error = engine.append(&other, vec![turn(1)], None).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::ResourceExhausted("tenants")));
    assert_eq!(error.outcome, Outcome::NotCommitted);
    drop(engine);
    let (tape, _) =
        Tape::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.tape.clone()).unwrap();
    let creates: Vec<_> =
        tape.scan(0).map(|r| r.unwrap()).filter(|r| r.kind == Kind::TenantCreate).collect();
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0].tenant_id, 1);
    assert_eq!(creates[1].tenant_id, 2);
    assert_eq!(creates[0].payload.as_ref(), b"tenant");
    drop(tape);
    let engine = open(disk, opts);
    engine.append(&scope("s"), vec![turn(2)], None).unwrap();
    assert_eq!(engine.window(&scope("s"), WindowQuery::Last(2)).unwrap().len(), 2);
    assert_eq!(engine.metrics().unwrap().tenants, 2);
}

#[test]
fn dedup_eviction_is_bounded_and_checkpointed() {
    let disk = Arc::new(SimDisk::new(4));
    let opts = EngineOptions { dedup_per_tenant: 2, dedup_global: 3, ..options() };
    let engine = open(disk.clone(), opts.clone());
    for i in 0..3 {
        engine.append(&scope("s"), vec![turn(i)], Some(&i.to_string())).unwrap();
    }
    assert_eq!(engine.metrics().unwrap().dedup_entries, 2);
    engine.checkpoint().unwrap();
    for i in 3..6 {
        engine.append(&scope("s"), vec![turn(i)], Some(&i.to_string())).unwrap();
    }
    engine.checkpoint().unwrap();
    drop(engine);
    let engine = open(disk, opts);
    assert_eq!(engine.metrics().unwrap().dedup_entries, 2);
    assert_eq!(engine.metrics().unwrap().dedup_window_effective, 2);
    assert_eq!(engine.append(&scope("s"), vec![turn(5)], Some("5")).unwrap().seqs, vec![6]);
    assert_eq!(engine.append(&scope("s"), vec![turn(0)], Some("0")).unwrap().seqs, vec![7]);
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
fn unknown_sync_outcome_poison_hides_reservations_and_keeps_reads() {
    for seed in 0..32 {
        let disk = Arc::new(SimDisk::new(seed));
        let engine = open(disk.clone(), options());
        let original = engine.append(&scope("s"), vec![turn(1)], Some("original")).unwrap();
        disk.fail_next_sync().unwrap();
        let error = engine.append(&scope("s"), vec![turn(2)], Some("unknown")).unwrap_err();
        assert_eq!(error.outcome, Outcome::Unknown);
        assert_eq!(engine.window(&scope("s"), WindowQuery::Last(10)).unwrap().len(), 1);
        assert_eq!(
            engine.append(&scope("s"), vec![turn(2)], Some("unknown")).unwrap_err().outcome,
            Outcome::NotCommitted
        );
        assert_eq!(engine.metrics().unwrap().durable_lsn, original.last_lsn);
        drop(engine);
        let reboot = Arc::new(disk.crash().unwrap());
        let engine = open(reboot, options());
        assert_eq!(engine.append(&scope("s"), vec![turn(1)], Some("original")).unwrap(), original);
        let retried = engine.append(&scope("s"), vec![turn(2)], Some("unknown")).unwrap();
        assert_eq!(retried.seqs, vec![2]);
        assert_eq!(engine.window(&scope("s"), WindowQuery::Last(10)).unwrap().len(), 2);
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
    engine.append(&scope("s"), vec![turn(2)], None).unwrap();
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
fn receipt_read_failure_reports_committed_and_checkpoint_io_uses_worker() {
    let io = Arc::new(support::ProbeIo::new(15));
    let engine =
        Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", options()).unwrap();
    io.fail_read.store(true, Ordering::SeqCst);
    let error = engine.append(&scope("s"), vec![turn(1)], Some("key")).unwrap_err();
    assert_eq!(error.outcome, Outcome::Committed);
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

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target").join(format!(
            "core-e2e-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
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
