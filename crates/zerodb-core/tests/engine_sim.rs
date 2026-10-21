#![cfg(feature = "sim")]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use zerodb_core::{
    turn::decode, Appended, Engine, EngineOptions, ErrorKind, Health, NewTurn, Outcome, Role,
    Scope, SimAppend, Turn, WindowQuery,
};
use zerodb_tape::sim::{CapacityError, CapacityPlan, PublishPause, SimClock, StorageFullFault};
use zerodb_tape::{Clock, Error, FileId, Io, Kind, LockGuard, Result, SimDisk, Tape, TapeOptions};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut n = self.0;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        n ^ (n >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

// Capture before the caller observes its reply. Later writes cannot alter the crash image.
struct FaultIo {
    disk: SimDisk,
    storage_full: StorageFullFault,
    watch_segment_writes: AtomicBool,
    segment_writes: AtomicUsize,
    cut: AtomicUsize,
    image: Mutex<Option<SimDisk>>,
    fail_before: AtomicBool,
    fail_after: AtomicBool,
    checkpoint_at: AtomicUsize,
    checkpoint: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    syncs: AtomicUsize,
}
impl FaultIo {
    fn new(disk: SimDisk) -> Self {
        Self {
            disk,
            storage_full: StorageFullFault::default(),
            watch_segment_writes: AtomicBool::new(false),
            segment_writes: AtomicUsize::new(0),
            cut: AtomicUsize::new(0),
            image: Mutex::new(None),
            fail_before: AtomicBool::new(false),
            fail_after: AtomicBool::new(false),
            checkpoint_at: AtomicUsize::new(0),
            checkpoint: Mutex::new(None),
            syncs: AtomicUsize::new(0),
        }
    }
    fn after<T>(&self, result: Result<T>) -> Result<T> {
        if self.cut.load(Ordering::SeqCst) > 0 && self.cut.fetch_sub(1, Ordering::SeqCst) == 1 {
            *self.image.lock().unwrap() = Some(self.disk.crash().unwrap());
        }
        if self.checkpoint_at.load(Ordering::SeqCst) > 0
            && self.checkpoint_at.fetch_sub(1, Ordering::SeqCst) == 1
        {
            let callback = self.checkpoint.lock().unwrap().take();
            if let Some(callback) = callback {
                callback();
            }
        }
        result
    }
    fn sync(&self, sync: impl FnOnce() -> Result<()>) -> Result<()> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        if self.fail_before.swap(false, Ordering::SeqCst) {
            self.disk.fail_next_sync()?;
        }
        let result = sync();
        let result = if self.fail_after.swap(false, Ordering::SeqCst) && result.is_ok() {
            Err(Error::SyncFailed("injected error after persistence".into()))
        } else {
            result
        };
        self.after(result)
    }
}
impl Io for FaultIo {
    fn create(&self, p: &Path) -> Result<FileId> {
        let file = self.after(self.disk.create(p))?;
        self.storage_full.opened(p, file);
        Ok(file)
    }
    fn open(&self, p: &Path) -> Result<FileId> {
        let file = self.disk.open(p)?;
        self.storage_full.opened(p, file);
        Ok(file)
    }
    fn close(&self, f: FileId) -> Result<()> {
        self.storage_full.closed(f);
        self.disk.close(f)
    }
    fn write_at(&self, f: FileId, o: u64, b: &[u8]) -> Result<()> {
        if self.watch_segment_writes.load(Ordering::SeqCst) && self.storage_full.is_segment(f) {
            self.segment_writes.fetch_add(1, Ordering::SeqCst);
        }
        self.storage_full.before_write(&self.disk, f, b.len());
        self.after(self.disk.write_at(f, o, b))
    }
    fn read_at(&self, f: FileId, o: u64, b: &mut [u8]) -> Result<usize> {
        self.disk.read_at(f, o, b)
    }
    fn len(&self, f: FileId) -> Result<u64> {
        self.disk.len(f)
    }
    fn set_len(&self, f: FileId, n: u64) -> Result<()> {
        self.after(self.disk.set_len(f, n))
    }
    fn allocate(&self, f: FileId, n: u64) -> Result<()> {
        self.after(self.disk.allocate(f, n))
    }
    fn available_bytes(&self, dir: &Path) -> Result<u64> {
        self.disk.available_bytes(dir)
    }
    fn total_bytes(&self, dir: &Path) -> Result<u64> {
        self.disk.total_bytes(dir)
    }
    fn sync_data(&self, f: FileId) -> Result<()> {
        self.storage_full.before_sync(&self.disk, f);
        self.sync(|| self.disk.sync_data(f))
    }
    fn sync_dir(&self, p: &Path) -> Result<()> {
        self.sync(|| self.disk.sync_dir(p))
    }
    fn rename(&self, a: &Path, b: &Path) -> Result<()> {
        self.after(self.disk.rename(a, b))
    }
    fn remove(&self, p: &Path) -> Result<()> {
        self.after(self.disk.remove(p))
    }
    fn list(&self, p: &Path) -> Result<Vec<PathBuf>> {
        self.disk.list(p)
    }
    fn exists(&self, p: &Path) -> Result<bool> {
        self.disk.exists(p)
    }
    fn try_lock_exclusive(&self, p: &Path) -> Result<LockGuard> {
        self.disk.try_lock_exclusive(p)
    }
}

#[derive(Clone)]
struct Batch {
    scope: usize,
    turns: Vec<NewTurn>,
    key: Option<String>,
    receipt: Option<Appended>,
    time: u64,
}
#[derive(Clone)]
struct Oracle {
    scopes: Vec<Scope>,
    history: Vec<Vec<Turn>>,
    batches: Vec<Batch>,
    tenants: BTreeMap<String, u32>,
}
impl Oracle {
    fn new() -> Self {
        let scopes = (0..48)
            .map(|i| Scope {
                tenant: format!("tenant-{}", i % 4),
                agent: format!("agent-{}", i / 24),
                session: format!("session-{}", i / 4 % 6),
            })
            .collect::<Vec<_>>();
        Self {
            history: vec![Vec::new(); scopes.len()],
            scopes,
            batches: Vec::new(),
            tenants: BTreeMap::new(),
        }
    }
    fn apply(&mut self, index: usize, receipt: Appended) {
        let previous =
            self.history.iter().flatten().map(|turn| turn.commit_time_ns).max().unwrap_or(0);
        let b = &mut self.batches[index];
        b.time = b.time.max(previous + 1);
        let history = &mut self.history[b.scope];
        assert_eq!(
            receipt.seqs,
            (history.len() as u64 + 1..=history.len() as u64 + b.turns.len() as u64)
                .collect::<Vec<_>>()
        );
        assert_eq!(receipt.last_lsn - receipt.first_lsn + 1, b.turns.len() as u64);
        for (i, turn) in b.turns.iter().enumerate() {
            history.push(Turn {
                seq: receipt.seqs[i],
                lsn: receipt.first_lsn + i as u64,
                commit_time_ns: b.time,
                role: turn.role,
                content: turn.content.clone(),
                meta: turn.meta.clone(),
            });
        }
        b.receipt = Some(receipt);
    }
    fn verify(&self, engine: &Engine, rng: &mut Rng) {
        for (scope, expected) in self.scopes.iter().zip(&self.history) {
            let before = engine.metrics().unwrap().window_reads;
            assert_eq!(engine.window(scope, WindowQuery::Last(usize::MAX)).unwrap(), *expected);
            assert_eq!(engine.metrics().unwrap().window_reads - before, expected.len() as u64);
            let n = rng.below(expected.len() + 5);
            assert_eq!(
                engine.window(scope, WindowQuery::Last(n)).unwrap(),
                expected[expected.len().saturating_sub(n)..]
            );
            let seq = rng.below(expected.len() + 5) as u64;
            let limit = rng.below(20);
            let start = (seq.max(1) - 1) as usize;
            let end = expected.len().min(start.saturating_add(limit));
            let wanted = &expected[start.min(expected.len())..end];
            assert_eq!(engine.window(scope, WindowQuery::Since { seq, limit }).unwrap(), wanted);
        }
        assert_eq!(engine.metrics().unwrap().tenants, self.tenants.len());
    }
    // Only the unresolved batch may extend the acknowledged prefix, and only as a whole.
    fn recover(
        &mut self,
        disk: &SimDisk,
        clock: Arc<SimClock>,
        opts: &EngineOptions,
        pending: &[usize],
    ) -> u64 {
        let (tape, _) = Tape::open(Arc::new(disk.clone()), clock, "db", opts.tape.clone()).unwrap();
        let mut actual = vec![Vec::new(); self.scopes.len()];
        let mut transactions: BTreeMap<u64, Vec<(u64, u64)>> = BTreeMap::new();
        let mut tenant_names = BTreeMap::new();
        let mut records = 0;
        for record in tape.scan(0) {
            let record = record.unwrap();
            records += 1;
            if record.kind == Kind::TenantCreate {
                let name = String::from_utf8(record.payload.to_vec()).unwrap();
                if let Some(id) = self.tenants.get(&name) {
                    assert_eq!(*id, record.tenant_id);
                }
                assert!(tenant_names.insert(record.tenant_id, name.clone()).is_none());
                self.tenants.insert(name, record.tenant_id);
                continue;
            }
            assert_eq!(record.kind, Kind::Turn);
            let decoded = decode(&record.payload).unwrap();
            let tenant = &tenant_names[&record.tenant_id];
            let scope = self
                .scopes
                .iter()
                .position(|s| {
                    &s.tenant == tenant
                        && s.agent == decoded.agent_id
                        && s.session == decoded.session_id
                })
                .unwrap();
            let batch = u64::from_le_bytes(decoded.turn.meta[..8].try_into().unwrap()) as usize;
            assert_eq!(self.batches[batch].scope, scope);
            assert_eq!(self.batches[batch].key, decoded.idem_key);
            assert!(
                self.batches[batch].receipt.is_some() || pending.contains(&batch),
                "NotCommitted or unwritten batch recovered: batch={batch}"
            );
            let turns = transactions.entry(record.txn).or_default();
            let input = &self.batches[batch].turns[turns.len()];
            assert_eq!(*input, decoded.turn);
            turns.push((batch as u64, record.lsn));
            actual[scope].push(Turn {
                seq: decoded.seq,
                lsn: record.lsn,
                commit_time_ns: record.commit_time_ns,
                role: decoded.turn.role,
                content: decoded.turn.content,
                meta: decoded.turn.meta,
            });
        }
        for (&txn, turns) in &transactions {
            let index = turns[0].0 as usize;
            assert!(turns.iter().all(|&(b, _)| b as usize == index));
            let batch = &self.batches[index];
            assert_eq!(turns.len(), batch.turns.len(), "partial transaction");
            let receipt = Appended {
                txn,
                first_lsn: turns[0].1,
                last_lsn: turns.last().unwrap().1,
                seqs: actual[batch.scope]
                    .iter()
                    .filter(|t| t.lsn >= turns[0].1 && t.lsn <= turns.last().unwrap().1)
                    .map(|t| t.seq)
                    .collect(),
            };
            if let Some(expected) = &batch.receipt {
                assert_eq!(*expected, receipt);
            } else {
                assert!(pending.contains(&index));
                self.apply(index, receipt);
            }
        }
        assert_eq!(actual, self.history, "acknowledged data lost or unexpected turns recovered");
        records
    }
}

fn open(io: Arc<FaultIo>, clock: Arc<SimClock>, opts: &EngineOptions) -> Engine {
    Engine::open_with(io, clock, "db", opts.clone()).unwrap()
}

fn retry(engine: &Engine, oracle: &mut Oracle, index: usize) {
    let batch = &oracle.batches[index];
    let before = engine.metrics().unwrap().durable_lsn;
    let receipt = engine
        .append(&oracle.scopes[batch.scope], batch.turns.clone(), batch.key.as_deref())
        .unwrap();
    if let Some(expected) = &batch.receipt {
        assert_eq!(*expected, receipt);
        assert_eq!(engine.metrics().unwrap().durable_lsn, before);
    } else {
        oracle.apply(index, receipt);
    }
    let scope = oracle.batches[index].scope;
    assert_eq!(
        engine.window(&oracle.scopes[scope], WindowQuery::Last(usize::MAX)).unwrap(),
        oracle.history[scope],
        "an acknowledged retry must be immediately readable"
    );
}

fn random_batch(rng: &mut Rng, index: usize, scope: usize, time: u64) -> Batch {
    let turns = (0..1 + rng.below(4))
        .map(|_| {
            let size = [0, 1, 31, 256, 1024, 8192][rng.below(6)];
            let meta = rng.below(64);
            let mut metadata = (index as u64).to_le_bytes().to_vec();
            metadata.extend(rng.bytes(meta));
            NewTurn {
                role: [Role::System, Role::User, Role::Assistant, Role::Tool][rng.below(4)],
                content: rng.bytes(size),
                meta: metadata,
            }
        })
        .collect();
    Batch {
        scope,
        turns,
        key: if rng.below(3) == 0 { None } else { Some(format!("key-{index}")) },
        receipt: None,
        time,
    }
}

fn corrupt_page_checksum(disk: &SimDisk, rng: &mut Rng) {
    let mut pages = Vec::new();
    for path in disk
        .list(Path::new("db"))
        .unwrap()
        .into_iter()
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with("ckpt-"))
    {
        let file = disk.open(&path).unwrap();
        let mut offset = 0;
        while offset < disk.len(file).unwrap() {
            let mut header = [0; 9];
            assert_eq!(disk.read_at(file, offset, &mut header).unwrap(), header.len());
            let len = u32::from_le_bytes(header[4..8].try_into().unwrap());
            if header[8] == 2 {
                pages.push((path.clone(), offset));
            }
            offset += u64::from(len);
        }
        disk.close(file).unwrap();
    }
    assert!(!pages.is_empty());
    let (path, offset) = &pages[rng.below(pages.len())];
    let file = disk.open(path).unwrap();
    disk.flip_bit_at(file, offset + rng.below(4) as u64, rng.below(8) as u8).unwrap();
    disk.close(file).unwrap();
}

fn run_seed(seed: u64) -> Vec<Vec<Turn>> {
    let plan = CapacityPlan::new(seed);
    match std::panic::catch_unwind(|| run_seed_with_capacity(seed, &plan)) {
        Ok(history) => history,
        Err(error) => {
            eprintln!("engine simulation seed={seed} fault_plan={plan:?}");
            std::panic::resume_unwind(error);
        }
    }
}

fn run_seed_with_capacity(seed: u64, plan: &CapacityPlan) -> Vec<Vec<Turn>> {
    let mut rng = Rng(seed);
    let opts = EngineOptions {
        tape: TapeOptions {
            segment_size: 64 * 1024,
            inline_sim: true,
            lsn_index_bytes: if seed.is_multiple_of(3) { 12 * 1024 } else { 8 * 1024 * 1024 },
            ..TapeOptions::default()
        },
        hot_page_bytes: if seed.is_multiple_of(2) { 4096 } else { 1024 * 1024 },
        checkpoint_bytes: 4096,
        ..EngineOptions::default()
    };
    let clock = Arc::new(SimClock::default());
    let mut io = Arc::new(FaultIo::new(SimDisk::new(seed)));
    let mut engine = open(io.clone(), clock.clone(), &opts);
    let mut oracle = Oracle::new();
    for generation in 0..3 {
        for step in 0..40 {
            clock.advance(1 + rng.below(2_000_000_000) as u64);
            // Seeded points replace the checkpoint timer. Memory-pressure flushes remain automatic.
            if rng.below(7) == 0 {
                engine.checkpoint().unwrap();
            }
            if !oracle.batches.is_empty() && rng.below(5) == 0 {
                let index = rng.below(oracle.batches.len());
                if oracle.batches[index].receipt.is_some() && oracle.batches[index].key.is_some() {
                    retry(&engine, &mut oracle, index);
                    let batch = &oracle.batches[index];
                    let mut changed = batch.turns.clone();
                    changed[0].content.push(0xff);
                    let before = engine.metrics().unwrap().durable_lsn;
                    let error = engine
                        .append(&oracle.scopes[batch.scope], changed, batch.key.as_deref())
                        .unwrap_err();
                    assert_eq!(error.outcome, Outcome::NotCommitted);
                    assert!(matches!(error.kind, ErrorKind::IdempotencyConflict));
                    assert_eq!(before, engine.metrics().unwrap().durable_lsn);
                }
            }
            let scope = if rng.below(3) == 0 { 0 } else { rng.below(oracle.scopes.len()) };
            let index = oracle.batches.len();
            let batch = random_batch(&mut rng, index, scope, clock.now_ns());
            // Every seed covers failed sync before and after persistence, plus live reopen.
            let fault = step == 12 || step == 28;
            if fault {
                if step == 12 {
                    io.fail_before.store(true, Ordering::SeqCst);
                } else {
                    io.fail_after.store(true, Ordering::SeqCst);
                }
            }
            let cut = step == 39;
            if cut {
                io.cut.store(1 + rng.below(6), Ordering::SeqCst);
            }
            let result =
                engine.append(&oracle.scopes[scope], batch.turns.clone(), batch.key.as_deref());
            oracle.batches.push(batch);
            let image = io.image.lock().unwrap().take();
            if image.is_some() {
                if let Ok(receipt) = &result {
                    let mut after_ack = oracle.clone();
                    after_ack.apply(index, receipt.clone());
                    after_ack.recover(&io.disk.crash().unwrap(), clock.clone(), &opts, &[]);
                }
            }
            let mut pending = Vec::new();
            if image.is_some() {
                if result.is_ok() || result.as_ref().is_err_and(|e| e.outcome == Outcome::Unknown) {
                    pending.push(index);
                }
            } else {
                match result {
                    Ok(receipt) => oracle.apply(index, receipt),
                    Err(error) if error.outcome == Outcome::Unknown => pending.push(index),
                    Err(error) => {
                        assert_eq!(error.outcome, Outcome::NotCommitted);
                        assert!(matches!(
                            error.kind,
                            ErrorKind::Storage(_) | ErrorKind::ResourceExhausted("hot_pages")
                        ));
                    }
                }
            }
            if fault || cut {
                io.cut.store(0, Ordering::SeqCst);
                io.fail_before.store(false, Ordering::SeqCst);
                io.fail_after.store(false, Ordering::SeqCst);
                // A captured image excludes graceful shutdown, unlike live reopen.
                let next = image.unwrap_or_else(|| {
                    if cut {
                        io.disk.crash().unwrap()
                    } else {
                        io.disk.clone()
                    }
                });
                drop(engine);
                oracle.recover(&next, clock.clone(), &opts, &pending);
                io = Arc::new(FaultIo::new(next));
                engine = open(io.clone(), clock.clone(), &opts);
                oracle.verify(&engine, &mut rng);
                // Unknown retries must either return the original receipt or install exactly once.
                for index in pending
                    .into_iter()
                    .filter(|&i| oracle.batches[i].key.is_some())
                    .collect::<Vec<_>>()
                {
                    oracle.batches[index].time = if oracle.batches[index].receipt.is_none() {
                        clock.now_ns()
                    } else {
                        oracle.batches[index].time
                    };
                    retry(&engine, &mut oracle, index);
                    retry(&engine, &mut oracle, index);
                }
            }
            if rng.below(8) == 0 {
                // Discover newly acknowledged tenant IDs through the independent tape scan at reopen.
                let expected = &oracle.history[scope];
                assert_eq!(
                    engine.window(&oracle.scopes[scope], WindowQuery::Last(7)).unwrap(),
                    expected[expected.len().saturating_sub(7)..]
                );
            }
            assert!(engine.metrics().unwrap().hot_page_bytes <= opts.hot_page_bytes);
        }
        // Cut checkpoint installation independently from append, including directory persistence.
        io.cut.store(1 + rng.below(14), Ordering::SeqCst);
        engine.checkpoint().unwrap();
        io.cut.store(0, Ordering::SeqCst);
        let next = io.image.lock().unwrap().take().unwrap_or_else(|| io.disk.crash().unwrap());
        drop(engine);
        oracle.recover(&next, clock.clone(), &opts, &[]);
        io = Arc::new(FaultIo::new(next));
        engine = open(io.clone(), clock.clone(), &opts);
        oracle.verify(&engine, &mut rng);
        engine.checkpoint().unwrap();
        drop(engine);
        // Checkpoints are derived data. Corrupt a referenced file and demand full replay.
        corrupt_page_checksum(&io.disk, &mut rng);
        let records = oracle.recover(&io.disk, clock.clone(), &opts, &[]);
        engine = open(io.clone(), clock.clone(), &opts);
        let metrics = engine.metrics().unwrap();
        assert!(metrics.checkpoint_warnings > 0, "generation={generation}");
        assert_eq!(metrics.replayed_records, records);
        oracle.verify(&engine, &mut rng);
        for index in 0..oracle.batches.len() {
            if oracle.batches[index].receipt.is_some()
                && oracle.batches[index].key.is_some()
                && rng.below(5) == 0
            {
                retry(&engine, &mut oracle, index);
            }
        }
    }
    let mut history = oracle.history;
    history.extend(grouped_seed(seed, plan));
    history
}

fn enqueue_keyed_batch<'a>(
    engine: &'a Engine,
    oracle: &mut Oracle,
    rng: &mut Rng,
    clock: &SimClock,
) -> (usize, SimAppend<'a>) {
    let index = oracle.batches.len();
    let scope = rng.below(4);
    let mut batch = random_batch(rng, index, scope, clock.now_ns());
    batch.turns.truncate(1);
    let len = 512 + rng.below(2048);
    batch.turns[0].content = rng.bytes(len);
    batch.key = Some(format!("key-{index}"));
    let reply = engine
        .enqueue_sim(&oracle.scopes[scope], batch.turns.clone(), batch.key.as_deref())
        .unwrap();
    oracle.batches.push(batch);
    (index, reply)
}

fn capacity_faults(
    engine: Arc<Engine>,
    io: &mut Arc<FaultIo>,
    clock: &Arc<SimClock>,
    opts: &EngineOptions,
    oracle: &mut Oracle,
    rng: &mut Rng,
    plan: &CapacityPlan,
) -> Arc<Engine> {
    let snapshot = |engine: &Engine, scopes: &[Scope]| {
        let metrics = engine.metrics().unwrap();
        let turns = scopes
            .iter()
            .map(|scope| engine.window(scope, WindowQuery::Last(usize::MAX)).unwrap())
            .collect::<Vec<_>>();
        (
            metrics.durable_lsn,
            metrics.tenants,
            metrics.dedup_entries,
            metrics.dedup_window_effective,
            turns,
        )
    };
    let before = snapshot(&engine, &oracle.scopes);
    let mut txn =
        oracle.batches.iter().filter_map(|b| b.receipt.as_ref().map(|r| r.txn)).max().unwrap();
    let mut indices = Vec::new();
    let mut queued = Vec::new();
    for _ in 0..2 + rng.below(3) {
        let (index, reply) = enqueue_keyed_batch(&engine, oracle, rng, clock);
        queued.push(reply);
        indices.push(index);
    }
    plan.limit(&io.disk);
    io.segment_writes.store(0, Ordering::SeqCst);
    io.watch_segment_writes.store(true, Ordering::SeqCst);
    engine.step_sim().unwrap();
    io.watch_segment_writes.store(false, Ordering::SeqCst);
    assert_eq!(io.segment_writes.load(Ordering::SeqCst), 0, "a floor rejection wrote to a segment");
    for reply in queued {
        let error = reply.finish().unwrap_err();
        assert!(matches!(error.kind, ErrorKind::Storage(Error::DiskFull)));
        assert_eq!(error.outcome, Outcome::NotCommitted);
    }
    assert_eq!(engine.health(), Health::ReadOnly, "a clean floor hit must not poison");
    let (scope, oldest) = oracle
        .history
        .iter()
        .enumerate()
        .filter_map(|(scope, turns)| turns.first().map(|turn| (scope, turn)))
        .min_by_key(|(_, turn)| turn.lsn)
        .unwrap();
    let capacity = io.disk.total_bytes(Path::new("db")).unwrap();
    let free = io.disk.available_bytes(Path::new("db")).unwrap();
    io.disk.set_capacity(capacity - free).unwrap();
    let reloads = engine.metrics().unwrap().tape_index.page_reloads;
    assert_eq!(
        engine
            .window(&oracle.scopes[scope], WindowQuery::Since { seq: oldest.seq, limit: 1 })
            .unwrap_or_else(|error| panic!("read-only cold read must not allocate: {error:?}")),
        vec![oldest.clone()],
        "acknowledged turn must remain readable with no free space"
    );
    if opts.tape.lsn_index_bytes == 4224 {
        assert!(
            engine.metrics().unwrap().tape_index.page_reloads > reloads,
            "sampled a cold index page"
        );
    }
    assert_eq!(io.disk.available_bytes(Path::new("db")).unwrap(), 0);
    assert_eq!(engine.health(), Health::ReadOnly, "cold reads must not clear read-only");
    io.disk.set_capacity(capacity).unwrap();
    if let Some(index) =
        oracle.batches.iter().position(|batch| batch.receipt.is_some() && batch.key.is_some())
    {
        retry(&engine, oracle, index);
        assert_eq!(engine.health(), Health::ReadOnly, "a committed retry does not recheck space");
    }
    assert_eq!(
        snapshot(&engine, &oracle.scopes),
        before,
        "a floor hit changed engine state or dedup"
    );

    plan.free(&io.disk);
    let mut lsn = before.0;
    for index in indices {
        retry(&engine, oracle, index);
        let receipt = oracle.batches[index].receipt.as_ref().unwrap();
        txn += 1;
        assert_eq!(receipt.txn, txn, "a floor rejection consumed a transaction");
        assert_eq!(receipt.first_lsn, lsn + 1, "a floor rejection consumed an LSN");
        assert_eq!(
            engine.metrics().unwrap().durable_lsn,
            receipt.last_lsn,
            "a rejected key returned a receipt without a durable write"
        );
        lsn = receipt.last_lsn;
        assert_eq!(engine.health(), Health::Ok, "freed space must clear read-only");
        retry(&engine, oracle, index);
    }
    // Retained keys must still return their original receipts after the rejection.
    for index in 0..oracle.batches.len() {
        if oracle.batches[index].receipt.is_some() && oracle.batches[index].key.is_some() {
            retry(&engine, oracle, index);
        }
    }

    let mut queued = Vec::new();
    for _ in 0..2 + rng.below(3) {
        queued.push(enqueue_keyed_batch(&engine, oracle, rng, clock));
    }
    io.storage_full.arm(plan);
    io.cut.store(plan.cut, Ordering::SeqCst);
    let full_before = io.disk.storage_full_returns().unwrap();
    engine.step_sim().unwrap();
    assert_eq!(engine.health(), Health::Poisoned, "post-prepare StorageFull must poison");
    assert_eq!(io.disk.storage_full_returns().unwrap() - full_before, 1);
    let image = io.image.lock().unwrap().take();
    let mut pending = Vec::new();
    let mut post_return_pending = Vec::new();
    for (position, (index, reply)) in queued.into_iter().enumerate() {
        let error = reply.finish().unwrap_err();
        let (class, outcomes) = plan.expected(position);
        assert!(
            match &error.kind {
                ErrorKind::Storage(error) => class.matches(error),
                ErrorKind::Poisoned => class == CapacityError::Poisoned,
                _ => false,
            },
            "{error:?}"
        );
        assert!(outcomes.contains(&error.outcome), "{error:?}");
        if error.outcome == Outcome::Unknown {
            post_return_pending.push(index);
        }
        if image.is_some() || error.outcome == Outcome::Unknown {
            pending.push(index);
        }
    }
    io.cut.store(0, Ordering::SeqCst);
    let crashed = io.disk.crash().unwrap();
    drop(engine);
    if image.is_some() {
        oracle.clone().recover(&crashed, clock.clone(), opts, &post_return_pending);
    }
    let next = image.unwrap_or(crashed);
    oracle.recover(&next, clock.clone(), opts, &pending);
    next.set_capacity(SimDisk::DEFAULT_CAPACITY).unwrap();
    *io = Arc::new(FaultIo::new(next));
    let engine = Arc::new(open(io.clone(), clock.clone(), opts));
    assert_eq!(engine.health(), Health::Ok);
    oracle.verify(&engine, rng);
    for index in pending {
        if oracle.batches[index].receipt.is_none() {
            oracle.batches[index].time = clock.now_ns();
        }
        retry(&engine, oracle, index);
        retry(&engine, oracle, index);
    }
    engine
}

fn grouped_seed(seed: u64, plan: &CapacityPlan) -> Vec<Vec<Turn>> {
    let mut rng = Rng(seed ^ 0x47b3_6f21_91e2_450a);
    let generations = 3;
    let capacity_generation = rng.below(generations);
    let opts = EngineOptions {
        tape: TapeOptions {
            segment_size: 64 * 1024,
            inline_sim: true,
            free_space_floor: Some(plan.floor),
            lsn_index_bytes: if seed.is_multiple_of(3) { 4224 } else { 8 * 1024 * 1024 },
            ..TapeOptions::default()
        },
        checkpoint_bytes: 8192,
        ..EngineOptions::default()
    };
    let clock = Arc::new(SimClock::default());
    let mut io = Arc::new(FaultIo::new(SimDisk::new(seed)));
    let mut engine = Arc::new(open(io.clone(), clock.clone(), &opts));
    let mut oracle = Oracle::new();
    for scope in 0..4 {
        let index = oracle.batches.len();
        let mut batch = random_batch(&mut rng, index, scope, clock.now_ns());
        batch.turns.truncate(1);
        let receipt = engine
            .append(&oracle.scopes[scope], batch.turns.clone(), batch.key.as_deref())
            .unwrap();
        oracle.batches.push(batch);
        oracle.apply(index, receipt);
    }

    fn frame_faults(seed: u64) {
        use zerodb_tape::sim::frame;
        for proof in [false, true] {
            let disk = Arc::new(SimDisk::new(seed));
            let opts = EngineOptions {
                tape: TapeOptions {
                    segment_size: 4096,
                    inline_sim: true,
                    ..TapeOptions::default()
                },
                checkpoint_bytes: u64::MAX,
                checkpoint_interval: std::time::Duration::from_secs(3600),
                ..EngineOptions::default()
            };
            let clock = Arc::new(SimClock::new(1000));
            let engine = Engine::open_with(disk.clone(), clock, "db", opts.clone()).unwrap();
            let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
            for index in 0..if proof { 3 } else { 1 } {
                engine
                    .append(
                        &scope,
                        vec![NewTurn { role: Role::User, content: vec![index], meta: vec![] }],
                        Some(&format!("key-{index}")),
                    )
                    .unwrap();
            }
            let image = Arc::new(disk.crash().unwrap());
            drop(engine);
            let path = Path::new("db/seg-0000000000000001.zdb");
            let file = image.open(path).unwrap();
            let mut bytes = vec![0; 4096];
            image.read_at(file, 0, &mut bytes).unwrap();
            let mut offset = 0;
            while let Ok((record, used)) = frame::decode(&bytes[offset..], 1, offset as u64) {
                if record.header.kind == Kind::Turn {
                    break;
                }
                offset += used;
            }
            image
                .flip_bit_at(
                    file,
                    (offset + if seed.is_multiple_of(2) { 4 } else { 40 }) as u64,
                    (seed % 8) as u8,
                )
                .unwrap();
            image.close(file).unwrap();
            let result = Engine::open_with(image, Arc::new(SimClock::new(0)), "db", opts);
            if proof {
                assert!(
                    matches!(
                        result,
                        Err(zerodb_core::EngineError {
                            kind: ErrorKind::Storage(Error::CorruptionDetected { lsn: 2, .. }),
                            ..
                        })
                    ),
                    "seed={seed}"
                );
            } else {
                let engine = result.unwrap();
                assert_eq!(engine.metrics().unwrap().durable_lsn, 0);
                assert!(engine.window(&scope, WindowQuery::Last(10)).unwrap().is_empty());
            }
        }
    }
    frame_faults(seed);
    for generation in 0..generations {
        for group in 0..8 {
            clock.advance(1 + rng.below(2_000_000_000) as u64);
            if generation == capacity_generation && group == plan.delay + 1 {
                engine =
                    capacity_faults(engine, &mut io, &clock, &opts, &mut oracle, &mut rng, plan);
            }
            let count = 2 + rng.below(3);
            let mut queued = Vec::new();
            for position in 0..count {
                let index = oracle.batches.len();
                let scope = rng.below(oracle.scopes.len());
                let mut batch = random_batch(&mut rng, index, scope, clock.now_ns());
                if position == 0 {
                    batch.key = Some(format!("key-{index}"));
                }
                queued.push((
                    index,
                    false,
                    engine
                        .enqueue_sim(
                            &oracle.scopes[scope],
                            batch.turns.clone(),
                            batch.key.as_deref(),
                        )
                        .unwrap(),
                ));
                if position == 0 {
                    queued.push((
                        index,
                        false,
                        engine
                            .enqueue_sim(
                                &oracle.scopes[scope],
                                batch.turns.clone(),
                                batch.key.as_deref(),
                            )
                            .unwrap(),
                    ));
                    let mut conflict = batch.turns.clone();
                    conflict[0].content.push(0xff);
                    queued.push((
                        index,
                        true,
                        engine
                            .enqueue_sim(&oracle.scopes[scope], conflict, batch.key.as_deref())
                            .unwrap(),
                    ));
                }
                oracle.batches.push(batch);
            }
            let fault = group == 6;
            let cut = group == 7;
            let checkpoint_fired = Arc::new(AtomicBool::new(false));
            let metrics = engine.metrics().unwrap();
            let boundary = metrics.durable_lsn;
            let writer_in_checkpoint = !fault
                && !cut
                && rng.below(2) == 0
                && metrics.durable_lsn > metrics.checkpoint_watermark;
            if !fault && !cut {
                let weak = Arc::downgrade(&engine);
                let fired = checkpoint_fired.clone();
                *io.checkpoint.lock().unwrap() = Some(Box::new(move || {
                    let engine = weak.upgrade().unwrap();
                    if writer_in_checkpoint {
                        engine.step_sim().unwrap();
                    } else {
                        assert_eq!(engine.metrics().unwrap().durable_lsn, boundary);
                        assert_eq!(engine.checkpoint().unwrap(), boundary);
                    }
                    fired.store(true, Ordering::SeqCst);
                }));
                io.checkpoint_at.store(1 + rng.below(count), Ordering::SeqCst);
            } else if fault {
                if generation % 2 == 0 {
                    io.fail_before.store(true, Ordering::SeqCst);
                } else {
                    io.fail_after.store(true, Ordering::SeqCst);
                }
            } else {
                io.cut.store(1 + rng.below(4), Ordering::SeqCst);
            }
            if writer_in_checkpoint {
                assert_eq!(engine.checkpoint().unwrap(), boundary);
                assert!(engine.metrics().unwrap().durable_lsn > boundary);
            } else {
                engine.step_sim().unwrap();
            }
            let results = queued
                .into_iter()
                .map(|(index, conflict, reply)| (index, conflict, reply.finish()))
                .collect::<Vec<_>>();
            if !fault && !cut {
                assert!(checkpoint_fired.load(Ordering::SeqCst));
            }
            io.checkpoint_at.store(0, Ordering::SeqCst);
            io.checkpoint.lock().unwrap().take();
            io.cut.store(0, Ordering::SeqCst);
            io.fail_before.store(false, Ordering::SeqCst);
            io.fail_after.store(false, Ordering::SeqCst);
            let image = io.image.lock().unwrap().take();
            if image.is_some() {
                let mut after_ack = oracle.clone();
                let mut uncertain = Vec::new();
                for (index, conflict, result) in &results {
                    if *conflict {
                        continue;
                    }
                    if let Ok(receipt) = result {
                        if let Some(original) = &after_ack.batches[*index].receipt {
                            assert_eq!(original, receipt);
                        } else {
                            after_ack.apply(*index, receipt.clone());
                        }
                    } else if result.as_ref().is_err_and(|e| e.outcome == Outcome::Unknown) {
                        uncertain.push(*index);
                    }
                }
                // Acknowledgements after the earlier cut must survive a later, post-return crash.
                after_ack.recover(&io.disk.crash().unwrap(), clock.clone(), &opts, &uncertain);
            }
            let mut pending = Vec::new();
            for (index, conflict, result) in results {
                if conflict {
                    let error = result.unwrap_err();
                    assert_eq!(error.outcome, Outcome::NotCommitted);
                    if !fault {
                        assert!(matches!(error.kind, ErrorKind::IdempotencyConflict));
                    }
                    continue;
                }
                match result {
                    Ok(receipt) if image.is_none() => {
                        if let Some(original) = &oracle.batches[index].receipt {
                            assert_eq!(*original, receipt);
                        } else {
                            oracle.apply(index, receipt);
                        }
                    }
                    Ok(_) => pending.push(index),
                    Err(error) if error.outcome == Outcome::Unknown => pending.push(index),
                    Err(error) => assert_eq!(error.outcome, Outcome::NotCommitted),
                }
            }
            pending.sort_unstable();
            pending.dedup();
            if fault || cut {
                let next = image.unwrap_or_else(|| {
                    if cut {
                        io.disk.crash().unwrap()
                    } else {
                        io.disk.clone()
                    }
                });
                drop(engine);
                oracle.recover(&next, clock.clone(), &opts, &pending);
                io = Arc::new(FaultIo::new(next));
                engine = Arc::new(open(io.clone(), clock.clone(), &opts));
                oracle.verify(&engine, &mut rng);
                for index in pending {
                    if oracle.batches[index].key.is_some() {
                        retry(&engine, &mut oracle, index);
                        retry(&engine, &mut oracle, index);
                    }
                }
            } else if rng.below(2) == 0 {
                engine.checkpoint_step_sim().unwrap();
            }
        }
    }
    oracle.history
}

#[test]
fn inline_group_publishes_once_and_replies_with_original_sequences() {
    let io = Arc::new(FaultIo::new(SimDisk::new(19)));
    let pause = Arc::new(PublishPause::default());
    let opts = EngineOptions {
        tape: TapeOptions {
            inline_sim: true,
            publish_pause: Some(pause.clone()),
            ..TapeOptions::default()
        },
        ..EngineOptions::default()
    };
    let engine = open(io.clone(), Arc::new(SimClock::default()), &opts);
    let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    let turn = NewTurn { role: Role::User, content: vec![1], meta: vec![] };
    engine.append(&scope, vec![turn.clone()], None).unwrap();
    let before = engine.metrics().unwrap();
    let syncs = io.syncs.load(Ordering::SeqCst);
    let first = engine.enqueue_sim(&scope, vec![turn.clone()], Some("same-group")).unwrap();
    let duplicate = engine.enqueue_sim(&scope, vec![turn.clone()], Some("same-group")).unwrap();
    let keyless = engine.enqueue_sim(&scope, vec![turn.clone()], None).unwrap();
    assert_eq!(engine.metrics().unwrap().durable_lsn, before.durable_lsn);
    let retried = std::thread::scope(|threads| {
        let (engine, scope, turn) = (&engine, &scope, &turn);
        let (arrived, release) = pause.arm();
        let publishing = threads.spawn(move || engine.step_sim().unwrap());
        arrived.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let parked = engine.metrics().unwrap();
        assert_eq!(parked.dedup_entries, before.dedup_entries + 1);
        assert_eq!(parked.durable_lsn, before.durable_lsn);
        assert!(engine.window(scope, WindowQuery::Since { seq: 2, limit: 1 }).unwrap().is_empty());

        let (started, starting) = mpsc::sync_channel(0);
        let (replied, reply) = mpsc::channel();
        let retrying = threads.spawn(move || {
            started.send(()).unwrap();
            replied.send(engine.append(scope, vec![turn.clone()], Some("same-group"))).unwrap();
        });
        starting.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(
            matches!(
                reply.recv_timeout(std::time::Duration::from_secs(1)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "retry returned before the tape watermark covered its receipt"
        );
        release.send(()).unwrap();
        publishing.join().unwrap();
        let retried = reply.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap();
        retrying.join().unwrap();
        let visible = engine.window(scope, WindowQuery::Since { seq: 2, limit: 1 }).unwrap();
        assert_eq!(visible.len(), 1, "an acknowledged concurrent retry must be readable");
        assert_eq!(visible[0].lsn, retried.last_lsn);
        assert_eq!(visible[0].content, turn.content);
        retried
    });
    let first = first.finish().unwrap();
    assert_eq!(first.seqs, vec![2]);
    assert_eq!(retried, first);
    assert_eq!(duplicate.finish().unwrap(), first);
    assert_eq!(keyless.finish().unwrap().seqs, vec![3]);
    assert_eq!(io.syncs.load(Ordering::SeqCst) - syncs, 1);
    let after = engine.metrics().unwrap();
    assert_eq!(after.writer_groups - before.writer_groups, 2);
    assert_eq!(after.writer_group_requests - before.writer_group_requests, 4);
    assert_eq!(engine.window(&scope, WindowQuery::Last(10)).unwrap().len(), 3);
}

#[test]
fn publish_during_checkpoint_keeps_the_new_page_dirty() {
    let io = Arc::new(FaultIo::new(SimDisk::new(31)));
    let clock = Arc::new(SimClock::default());
    let opts = EngineOptions {
        tape: TapeOptions { segment_size: 64 * 1024, inline_sim: true, ..TapeOptions::default() },
        ..EngineOptions::default()
    };
    let engine = Arc::new(open(io.clone(), clock.clone(), &opts));
    let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    let turn = |value| NewTurn { role: Role::User, content: vec![value], meta: vec![] };
    let before = engine.append(&scope, vec![turn(1), turn(2)], None).unwrap();
    let queued = engine.enqueue_sim(&scope, vec![turn(3)], None).unwrap();
    let weak = Arc::downgrade(&engine);
    let fired = Arc::new(AtomicBool::new(false));
    let completed = fired.clone();
    *io.checkpoint.lock().unwrap() = Some(Box::new(move || {
        weak.upgrade().unwrap().step_sim().unwrap();
        completed.store(true, Ordering::SeqCst);
    }));
    io.checkpoint_at.store(2, Ordering::SeqCst);
    assert_eq!(engine.checkpoint().unwrap(), before.last_lsn);
    assert!(fired.load(Ordering::SeqCst));
    let receipt = queued.finish().unwrap();
    assert_eq!(receipt.seqs, vec![3]);
    assert_eq!(engine.checkpoint().unwrap(), receipt.last_lsn);
    drop(engine);
    let engine = open(io, clock, &opts);
    let window = engine.window(&scope, WindowQuery::Last(10)).unwrap();
    assert_eq!(window.iter().map(|t| t.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(
        window.iter().map(|t| t.content.clone()).collect::<Vec<_>>(),
        vec![vec![1], vec![2], vec![3]]
    );
    assert_eq!(engine.metrics().unwrap().replayed_records, 0);
}

#[test]
fn legacy_checkpoint_magic_falls_back_to_full_replay() {
    let io = Arc::new(FaultIo::new(SimDisk::new(33)));
    let clock = Arc::new(SimClock::default());
    let opts = EngineOptions {
        tape: TapeOptions { segment_size: 64 * 1024, inline_sim: true, ..TapeOptions::default() },
        ..EngineOptions::default()
    };
    let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    let turn = NewTurn { role: Role::User, content: vec![7], meta: vec![] };
    let engine = open(io.clone(), clock.clone(), &opts);
    engine.append(&scope, vec![turn.clone(); 2], Some("original")).unwrap();
    engine.checkpoint().unwrap();
    drop(engine);
    let path = io
        .disk
        .list(Path::new("db"))
        .unwrap()
        .into_iter()
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("ckpt-"))
        .unwrap();
    let file = io.disk.open(&path).unwrap();
    let mut header = [0; 28];
    assert_eq!(io.disk.read_at(file, 0, &mut header).unwrap(), header.len());
    header[8..12].copy_from_slice(b"ZCK1");
    let crc = zerodb_tape::sim::crc32c(&header[4..]);
    header[..4].copy_from_slice(&crc.to_le_bytes());
    io.disk.write_at(file, 0, &header).unwrap();
    io.disk.sync_data(file).unwrap();
    io.disk.close(file).unwrap();
    let engine = open(io, clock, &opts);
    assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 1);
    assert_eq!(engine.metrics().unwrap().replayed_records, 3);
    assert_eq!(engine.window(&scope, WindowQuery::Last(10)).unwrap().len(), 2);
    assert_eq!(engine.append(&scope, vec![turn; 2], Some("original")).unwrap().seqs, vec![1, 2]);
}

#[test]
fn missing_checkpoint_filename_after_crash_replays_the_tape() {
    let opts = EngineOptions {
        tape: TapeOptions { segment_size: 64 * 1024, inline_sim: true, ..TapeOptions::default() },
        ..EngineOptions::default()
    };
    let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    let turn = NewTurn { role: Role::User, content: vec![9], meta: vec![] };
    let mut observed = false;
    for seed in 0..128 {
        eprintln!("case: seed={seed}");
        let io = Arc::new(FaultIo::new(SimDisk::new(seed)));
        let clock = Arc::new(SimClock::default());
        let engine = open(io.clone(), clock.clone(), &opts);
        let receipt = engine.append(&scope, vec![turn.clone(); 2], None).unwrap();
        // Capture after manifest rename, before the sole checkpoint directory sync.
        io.cut.store(7, Ordering::SeqCst);
        engine.checkpoint().unwrap();
        let image = io.image.lock().unwrap().take().unwrap();
        drop(engine);
        if image.exists(Path::new("db/CHECKPOINT")).unwrap()
            && !image
                .exists(&Path::new("db").join(format!("ckpt-{}.zdb", receipt.last_lsn)))
                .unwrap()
        {
            let engine = Engine::open_with(Arc::new(image), clock, "db", opts.clone()).unwrap();
            assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 1, "seed={seed}");
            assert_eq!(engine.metrics().unwrap().replayed_records, 3, "seed={seed}");
            assert_eq!(
                engine.window(&scope, WindowQuery::Last(10)).unwrap().len(),
                2,
                "seed={seed}"
            );
            observed = true;
            break;
        }
    }
    assert!(observed, "seeds=0..128: no missing-reference crash image was exercised");
}

#[test]
fn seeded_engine_recovery() {
    let first = std::env::var("ZERODB_SIM_SEED").map_or(0, |v| v.parse::<u64>().unwrap());
    let count = std::env::var("ZERODB_SIM_SEEDS").map_or_else(
        |_| if std::env::var_os("ZERODB_SIM_SEED").is_some() { 1 } else { 500 },
        |v| v.parse::<u64>().unwrap(),
    );
    assert!(count > 0, "ZERODB_SIM_SEEDS must be positive");
    for offset in 0..count {
        let seed = first.wrapping_add(offset);
        if let Err(error) = std::panic::catch_unwind(|| run_seed(seed)) {
            eprintln!("engine simulation seed={seed}\nrepro: ZERODB_SIM_SEED={seed} cargo test -p zerodb-core --release --features sim --test engine_sim");
            std::panic::resume_unwind(error);
        }
    }
    eprintln!("engine simulation passed: {count} seeds starting at {first}");
}
