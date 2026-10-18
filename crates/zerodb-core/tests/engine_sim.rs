#![cfg(feature = "sim")]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use zerodb_core::{
    turn::decode, Appended, Engine, EngineOptions, ErrorKind, NewTurn, Outcome, Role, Scope, Trust,
    Turn, WindowQuery,
};
use zerodb_tape::sim::SimClock;
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
        self.after(self.disk.create(p))
    }
    fn open(&self, p: &Path) -> Result<FileId> {
        self.disk.open(p)
    }
    fn close(&self, f: FileId) -> Result<()> {
        self.disk.close(f)
    }
    fn write_at(&self, f: FileId, o: u64, b: &[u8]) -> Result<()> {
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
    fn sync_data(&self, f: FileId) -> Result<()> {
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
        let b = &mut self.batches[index];
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
                trust: turn.trust,
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
            assert!(self.batches[batch].receipt.is_some() || pending.contains(&batch));
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
                trust: decoded.turn.trust,
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
                trust: if rng.below(2) == 0 { Trust::Trusted } else { Trust::Untrusted },
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
    let mut rng = Rng(seed);
    let opts = EngineOptions {
        tape: TapeOptions { segment_size: 64 * 1024, inline_sim: true, ..TapeOptions::default() },
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
    history.extend(grouped_seed(seed));
    history
}

fn grouped_seed(seed: u64) -> Vec<Vec<Turn>> {
    let mut rng = Rng(seed ^ 0x47b3_6f21_91e2_450a);
    let opts = EngineOptions {
        tape: TapeOptions { segment_size: 64 * 1024, inline_sim: true, ..TapeOptions::default() },
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
    for generation in 0..3 {
        for group in 0..8 {
            clock.advance(1 + rng.below(2_000_000_000) as u64);
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
    let opts = EngineOptions {
        tape: TapeOptions { inline_sim: true, ..TapeOptions::default() },
        ..EngineOptions::default()
    };
    let engine = open(io.clone(), Arc::new(SimClock::default()), &opts);
    let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    let turn =
        NewTurn { role: Role::User, trust: Trust::Untrusted, content: vec![1], meta: vec![] };
    engine.append(&scope, vec![turn.clone()], None).unwrap();
    let before = engine.metrics().unwrap();
    let syncs = io.syncs.load(Ordering::SeqCst);
    let first = engine.enqueue_sim(&scope, vec![turn.clone()], Some("same-group")).unwrap();
    let duplicate = engine.enqueue_sim(&scope, vec![turn.clone()], Some("same-group")).unwrap();
    let keyless = engine.enqueue_sim(&scope, vec![turn], None).unwrap();
    assert_eq!(engine.metrics().unwrap().durable_lsn, before.durable_lsn);
    engine.step_sim().unwrap();
    let first = first.finish().unwrap();
    assert_eq!(first.seqs, vec![2]);
    assert_eq!(duplicate.finish().unwrap(), first);
    assert_eq!(keyless.finish().unwrap().seqs, vec![3]);
    assert_eq!(io.syncs.load(Ordering::SeqCst) - syncs, 1);
    assert_eq!(engine.metrics().unwrap().writer_groups - before.writer_groups, 1);
    assert_eq!(engine.metrics().unwrap().writer_group_requests - before.writer_group_requests, 3);
    assert_eq!(engine.window(&scope, WindowQuery::Last(10)).unwrap().len(), 3);
}

#[test]
fn oracle_rejects_partial_unknown_batches_and_missing_acknowledgements() {
    for partial in [false, true] {
        let opts = EngineOptions {
            tape: TapeOptions { inline_sim: true, ..TapeOptions::default() },
            ..EngineOptions::default()
        };
        let clock = Arc::new(SimClock::default());
        let io = Arc::new(FaultIo::new(SimDisk::new(21)));
        let engine = open(io.clone(), clock.clone(), &opts);
        let before = io.disk.crash().unwrap();
        let mut oracle = Oracle::new();
        let mut batch = random_batch(&mut Rng(21), 0, 0, 0);
        batch.turns.truncate(1);
        let receipt =
            engine.append(&oracle.scopes[0], batch.turns.clone(), batch.key.as_deref()).unwrap();
        if partial {
            batch.turns.push(batch.turns[0].clone());
        }
        oracle.batches.push(batch);
        if !partial {
            oracle.apply(0, receipt);
        }
        let image = if partial { io.disk.crash().unwrap() } else { before };
        drop(engine);
        let pending = if partial { vec![0] } else { vec![] };
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            oracle.recover(&image, clock, &opts, &pending);
        }))
        .is_err());
    }
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
    let turn = |value| NewTurn {
        role: Role::User,
        trust: Trust::Untrusted,
        content: vec![value],
        meta: vec![],
    };
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
    let turn =
        NewTurn { role: Role::User, trust: Trust::Untrusted, content: vec![7], meta: vec![] };
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
    let turn =
        NewTurn { role: Role::User, trust: Trust::Untrusted, content: vec![9], meta: vec![] };
    let mut observed = false;
    for seed in 0..128 {
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
            assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 1);
            assert_eq!(engine.metrics().unwrap().replayed_records, 3);
            assert_eq!(engine.window(&scope, WindowQuery::Last(10)).unwrap().len(), 2);
            observed = true;
            break;
        }
    }
    assert!(observed, "no missing-reference crash image was exercised");
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
            eprintln!("engine simulation seed={seed}\nrepro: ZERODB_SIM_SEED={seed} cargo test -p zerodb-core --release --features sim --test engine_sim -- --nocapture");
            std::panic::resume_unwind(error);
        }
    }
    eprintln!("engine simulation passed: {count} seeds starting at {first}");
}

#[test]
fn engine_simulation_is_reproducible() {
    assert_eq!(run_seed(19), run_seed(19));
}
