use crate::checkpoint::{self, Worker};
use crate::dedup::DedupKey;
use crate::error::{lock, Result};
use crate::machine::Machine;
use crate::session::{address, Key};
use crate::state::State;
use crate::turn::{decode, encode};
use crate::{EngineError, EngineOptions, ErrorKind, NewTurn, Role};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use zerodb_tape::{Clock, Committed, Durability, Io, Kind, RecordInput, StdIo, SystemClock, Tape};

/// A session's isolation boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    /// Tenant name, at most 255 UTF-8 bytes.
    pub tenant: String,
    /// Agent identifier, at most 255 UTF-8 bytes.
    pub agent: String,
    /// Session identifier, at most 255 UTF-8 bytes.
    pub session: String,
}
/// A durable append receipt. LSNs cover turns only, excluding tenant creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Appended {
    /// Original transaction, including on retries.
    pub txn: u64,
    /// First turn's LSN.
    pub first_lsn: u64,
    /// Last turn's LSN.
    pub last_lsn: u64,
    /// Dense session sequences, starting at 1.
    pub seqs: Vec<u64>,
}
/// A durable turn returned in sequence order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Turn {
    /// Stable tape address.
    pub lsn: u64,
    /// Dense sequence within the scope.
    pub seq: u64,
    /// Commit timestamp from the tape.
    pub commit_time_ns: u64,
    /// Speaker role.
    pub role: Role,
    /// Opaque content.
    pub content: Vec<u8>,
    /// Opaque metadata.
    pub meta: Vec<u8>,
}
/// An ordered session window.
#[derive(Clone, Copy, Debug)]
pub enum WindowQuery {
    /// Return at most this many recent turns.
    Last(usize),
    /// Return from this inclusive sequence, capped by limit. Sequence 0 starts at 1.
    Since {
        /// Inclusive starting sequence.
        seq: u64,
        /// Maximum number of turns.
        limit: usize,
    },
}
/// Observable bounds and recovery counters.
#[derive(Clone, Debug)]
pub struct Metrics {
    /// Highest readable tape LSN.
    pub durable_lsn: u64,
    /// Tape-index resident memory, reloads and eviction counters.
    pub tape_index: zerodb_tape::LsnIndexStats,
    /// Resident session-slot capacity.
    pub hot_page_bytes: usize,
    /// Retained published retry keys.
    pub dedup_entries: usize,
    /// Smallest retained per-tenant key count.
    pub dedup_window_effective: usize,
    /// Installed checkpoint boundary.
    pub checkpoint_watermark: u64,
    /// Installed incremental files.
    pub checkpoint_files: usize,
    /// Completed checkpoint installs during this open.
    pub checkpoint_saves: u64,
    /// Maximum occupied encoding buffer among completed checkpoints, bounded at 4 MiB.
    pub checkpoint_buffer_peak_bytes: usize,
    /// Published encoded-byte accounting since the installed checkpoint, within this process.
    pub checkpoint_pending_bytes: u64,
    /// Writer steps containing queued requests.
    pub writer_groups: u64,
    /// Requests handled by those steps, including cached replies and rejections.
    pub writer_group_requests: u64,
    /// Successful runtime superblock installs during this open.
    pub superblock_installs: u64,
    /// Application records replayed during this open.
    pub replayed_records: u64,
    /// Checkpoints rejected during this open.
    pub checkpoint_warnings: u64,
    /// Maximum checkpoint snapshot lock hold, in nanoseconds. Excludes lock acquisition.
    pub checkpoint_snapshot_lock_ns: u64,
    /// Maximum State hold across checkpoint snapshot, install chunks, and watermark publication.
    /// Excludes lock acquisition and includes all work inside each critical section.
    pub checkpoint_state_lock_ns: u64,
    /// Tape::read calls issued by window, excluding append receipts.
    pub window_reads: u64,
    /// Registered tenants.
    pub tenants: usize,
}

/// Writer availability. Reads retain the last durable view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Health {
    /// No write-blocking failure has been observed.
    Ok,
    /// A space check failed or preallocation ran out of space. A later write retries.
    ReadOnly,
    /// Storage or shared state failed. Reopen before writing again.
    Poisoned,
}

/// Durable session storage. Drop joins the checkpoint worker before closing the tape.
pub struct Engine {
    worker: Worker,
    tape: Tape<Machine>,
    state: Arc<Mutex<State>>,
    failed: Arc<AtomicBool>,
    io: Arc<dyn Io>,
    dir: PathBuf,
    tenant_creation: Mutex<()>,
    reads: AtomicU64,
}
impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("path", &self.dir)
            .field("tape", &self.tape)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Returns writer health without blocking or performing I/O.
    /// Poisoning takes precedence over read-only mode.
    ///
    /// Only a later write that passes its space check clears `Health::ReadOnly`.
    /// Keep sending writes or retries to read-only instances. Don't drain them.
    pub fn health(&self) -> Health {
        if self.tape.is_poisoned()
            || self.failed.load(Ordering::Acquire)
            || matches!(self.state.try_lock(), Err(TryLockError::Poisoned(_)))
        {
            Health::Poisoned
        } else if self.tape.read_only() {
            Health::ReadOnly
        } else {
            Health::Ok
        }
    }

    /// Creates the directory, recovers its tape, and starts background workers using real filesystem I/O.
    pub fn open(path: impl AsRef<Path>, opts: EngineOptions) -> Result<Self> {
        std::fs::create_dir_all(path.as_ref()).map_err(zerodb_tape::Error::from)?;
        Self::open_with(Arc::new(StdIo::new()), Arc::new(SystemClock), path, opts)
    }
    /// Opens through injected storage and time. Real directories must already exist.
    pub fn open_with(
        io: Arc<dyn Io>,
        clock: Arc<dyn Clock>,
        path: impl AsRef<Path>,
        mut opts: EngineOptions,
    ) -> Result<Self> {
        if opts.hot_page_bytes < 128
            || opts.max_sessions == 0
            || opts.max_pages == 0
            || opts.max_tenants == 0
            || opts.max_tenants > 10_000
            || opts.dedup_per_tenant == 0
            || opts.dedup_global == 0
            || opts.checkpoint_interval.is_zero()
        {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let dir = path.as_ref().to_owned();
        let state = Arc::new(Mutex::new(checkpoint::load(io.as_ref(), &dir, &opts)));
        opts.tape.replay_from_lsn = lock(&state)?.watermark;
        let machine = Machine::new(state.clone(), io.clone(), dir.clone(), opts.clone())?;
        let opened = Tape::open_with(machine, io.clone(), clock.clone(), &dir, opts.tape.clone());
        let retry = opts.tape.replay_from_lsn > 0
            && match &opened {
                Ok((_, report)) => report.durable_lsn < opts.tape.replay_from_lsn,
                Err(_) => lock(&state)?.replay_failed,
            };
        let tape = if retry {
            drop(opened);
            let mut fresh = State::new(&opts);
            fresh.warnings = 1;
            {
                let mut previous = lock(&state)?;
                fresh.checkpoint_attempt = previous.checkpoint_attempt;
                *previous = fresh;
            }
            eprintln!("zerodb: checkpoint inconsistent with recovered tape, replaying tape");
            opts.tape.replay_from_lsn = 0;
            Tape::open_with(
                Machine::new(state.clone(), io.clone(), dir.clone(), opts.clone())?,
                io.clone(),
                clock,
                &dir,
                opts.tape.clone(),
            )?
            .0
        } else {
            opened?.0
        };
        let chain = lock(&state)?.chain.clone();
        checkpoint::sweep(io.as_ref(), &dir, &chain)?;
        let worker = Worker::start(io.clone(), dir.clone(), state.clone(), &opts)?;
        lock(&state)?.notify = worker.sender.clone();
        let failed = lock(&state)?.failed.clone();
        Ok(Self {
            worker,
            tape,
            state,
            failed,
            io,
            dir,
            tenant_creation: Mutex::new(()),
            reads: AtomicU64::new(0),
        })
    }

    /// Atomically appends turns and returns only after durable publication.
    ///
    /// Retry keys are scoped by tenant and session, not agent. A changed payload is a conflict.
    pub fn append(
        &self,
        scope: &Scope,
        turns: Vec<NewTurn>,
        idem_key: Option<&str>,
    ) -> Result<Appended> {
        if scope.tenant.len() > 255 || turns.is_empty() {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let payloads = turns
            .iter()
            .map(|turn| encode(&scope.agent, &scope.session, 0, turn, idem_key))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let digest = idem_key.map(|_| {
            let mut hash = blake3::Hasher::new();
            for payload in &payloads {
                hash.update(&(payload.len() as u32).to_le_bytes());
                hash.update(payload);
            }
            *hash.finalize().as_bytes()
        });
        let mut tenant = {
            let state = lock(&self.state)?;
            let tenant = state.tenants.get(&scope.tenant);
            if let (Some(tenant), Some(idem_key), Some(digest)) = (tenant, idem_key, digest) {
                let key = DedupKey(tenant, scope.session.clone(), idem_key.to_owned());
                // Only published entries bypass the writer. Prepare still resolves in-flight duplicates.
                if let Some(entry) = state.dedup.get(&key) {
                    if let Some(error) = &state.failure {
                        return Err(error.clone());
                    }
                    let Committed::Durable { txn, last_lsn, reply, .. } = entry.resolve(digest)?;
                    drop(state);
                    return self.receipt(turns.len() as u64, txn, last_lsn, &reply);
                }
            }
            tenant
        };
        let _creation = if tenant.is_none() {
            let guard = lock(&self.tenant_creation)?;
            tenant = lock(&self.state)?.tenants.get(&scope.tenant);
            Some(guard)
        } else {
            None
        };
        let mut records = Vec::new();
        if tenant.is_none() {
            records.push(RecordInput {
                kind: Kind::TenantCreate,
                tenant_id: 0,
                content: scope.tenant.as_bytes().to_vec(),
            });
        }
        records.extend(payloads.into_iter().map(|content| RecordInput {
            kind: Kind::Turn,
            tenant_id: tenant.unwrap_or(0),
            content,
        }));
        for attempt in 0..2 {
            let pin = tenant
                .map(|id| self.pin(Key(id, scope.agent.clone(), scope.session.clone())))
                .transpose();
            let pin = match pin {
                Err(EngineError { kind: ErrorKind::ResourceExhausted("hot_pages"), .. })
                    if attempt == 0 =>
                {
                    self.worker.flush()?;
                    continue;
                }
                result => result?,
            };
            let result = self
                .tape
                .append_with_digest(records.clone(), Durability::Durable, digest)
                .map_err(EngineError::write);
            drop(pin);
            match result {
                Err(EngineError { kind: ErrorKind::ResourceExhausted("hot_pages"), .. })
                    if attempt == 0 =>
                {
                    self.worker.flush()?;
                }
                Err(error) => return Err(error),
                Ok(Committed::Durable { txn, last_lsn, reply, .. }) => {
                    return self.receipt(turns.len() as u64, txn, last_lsn, &reply);
                }
            }
        }
        Err(EngineError::rejected(ErrorKind::ResourceExhausted("hot_pages")))
    }

    fn receipt(&self, count: u64, txn: u64, last_lsn: u64, reply: &[u8]) -> Result<Appended> {
        (|| {
            if let Some(error) = &lock(&self.state)?.failure {
                return Err(error.clone());
            }
            let seq = u64::from_le_bytes(
                reply.try_into().map_err(|_| EngineError::rejected(ErrorKind::CorruptIndex))?,
            );
            let end = seq
                .checked_add(count)
                .ok_or_else(|| EngineError::rejected(ErrorKind::CorruptIndex))?;
            if seq == 0 || last_lsn < count {
                return Err(EngineError::rejected(ErrorKind::CorruptIndex));
            }
            Ok(Appended {
                txn,
                first_lsn: last_lsn - count + 1,
                last_lsn,
                seqs: (seq..end).collect(),
            })
        })()
        .map_err(|mut error: EngineError| {
            error.outcome = zerodb_tape::Outcome::Committed;
            error
        })
    }

    /// Queues an append for a later inline step. Register its tenant with append first.
    #[cfg(feature = "sim")]
    pub fn enqueue_sim(
        &self,
        scope: &Scope,
        turns: Vec<NewTurn>,
        idem_key: Option<&str>,
    ) -> Result<SimAppend<'_>> {
        if turns.is_empty() || scope.tenant.len() > 255 {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let tenant = lock(&self.state)?
            .tenants
            .get(&scope.tenant)
            .ok_or_else(|| EngineError::rejected(ErrorKind::InvalidInput))?;
        let records = turns
            .iter()
            .map(|turn| {
                Ok(RecordInput {
                    kind: Kind::Turn,
                    tenant_id: tenant,
                    content: encode(&scope.agent, &scope.session, 0, turn, idem_key)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let digest = idem_key.map(|_| {
            let mut hash = blake3::Hasher::new();
            for record in &records {
                hash.update(&(record.content.len() as u32).to_le_bytes());
                hash.update(&record.content);
            }
            *hash.finalize().as_bytes()
        });
        let key = Key(tenant, scope.agent.clone(), scope.session.clone());
        let pin = match self.pin(key.clone()) {
            Err(EngineError { kind: ErrorKind::ResourceExhausted("hot_pages"), .. }) => {
                self.worker.flush()?;
                self.pin(key)?
            }
            result => result?,
        };
        let reply = self.tape.enqueue_sim(records, digest)?;
        Ok(SimAppend { engine: self, _pin: pin, count: turns.len() as u64, reply })
    }

    /// Runs the queued inline group. Replies become available after sync and publication.
    #[cfg(feature = "sim")]
    pub fn step_sim(&self) -> Result<()> {
        self.tape.step_sim().map_err(EngineError::from)
    }

    /// Services one pending byte-threshold checkpoint through the production wake logic.
    #[cfg(feature = "sim")]
    pub fn checkpoint_step_sim(&self) -> Result<u64> {
        self.worker.wake_sim()
    }

    fn pin(&self, key: Key) -> Result<Pin<'_>> {
        let mut state = lock(&self.state)?;
        let mut active = false;
        let len = if let Some(entry) = state.index.sessions.get_mut(&key) {
            entry.pins += 1;
            active = true;
            entry.reserved
        } else {
            0
        };
        drop(state);
        let pin = Pin { engine: self, key, active };
        if len > 0 {
            self.load_page(&pin.key, address(len).0)?;
        }
        Ok(pin)
    }

    fn load_page(&self, key: &Key, page: usize) -> Result<Arc<Vec<u64>>> {
        loop {
            let backing = {
                let mut state = lock(&self.state)?;
                if let Some(slots) = state.index.resident(key, page)? {
                    return Ok(slots);
                }
                state.index.backing(key, page)?
            };
            let read = checkpoint::read_page(self.io.as_ref(), &self.dir, &backing);
            let mut state = lock(&self.state)?;
            if let Some(slots) = state.index.resident(key, page)? {
                return Ok(slots);
            }
            if state.index.backing(key, page)? != backing {
                continue;
            }
            if state.index.install_loaded(key, page, &backing, read?)? {
                return state
                    .index
                    .resident(key, page)?
                    .ok_or_else(|| EngineError::rejected(ErrorKind::CorruptIndex));
            }
        }
    }

    /// Reads an ordered window at a pinned durable boundary, with one tape read per returned turn.
    pub fn window(&self, scope: &Scope, query: WindowQuery) -> Result<Vec<Turn>> {
        match self.window_snapshot(scope, query) {
            Err(EngineError { kind: ErrorKind::ResourceExhausted("hot_pages"), .. }) => {
                self.worker.flush()?;
                self.window_snapshot(scope, query)
            }
            result => result,
        }
    }
    fn window_snapshot(&self, scope: &Scope, query: WindowQuery) -> Result<Vec<Turn>> {
        let state = lock(&self.state)?;
        let snapshot = self.tape.durable_lsn();
        let Some(tenant) = state.tenants.get(&scope.tenant) else {
            return Ok(Vec::new());
        };
        let key = Key(tenant, scope.agent.clone(), scope.session.clone());
        let Some(entry) = state.index.sessions.get(&key) else {
            return Ok(Vec::new());
        };
        let end = entry.len.saturating_add(1);
        let (start, end) = match query {
            WindowQuery::Last(n) => (end.saturating_sub(n as u64).max(1), end),
            WindowQuery::Since { seq, limit } => {
                (seq.max(1).min(end), end.min(seq.max(1).saturating_add(limit as u64)))
            }
        };
        drop(state);
        let mut slots = Vec::new();
        let mut seq = start;
        while seq < end {
            let (page, offset) = address(seq);
            let loaded = self.load_page(&key, page)?;
            let available = loaded
                .len()
                .checked_sub(offset)
                .ok_or_else(|| EngineError::rejected(ErrorKind::CorruptIndex))?;
            let count = (end - seq).min(available as u64) as usize;
            if count == 0 {
                return Err(EngineError::rejected(ErrorKind::CorruptIndex));
            }
            slots.extend_from_slice(&loaded[offset..offset + count]);
            seq += count as u64;
        }
        let mut out = Vec::with_capacity(slots.len());
        for (offset, lsn) in slots.into_iter().enumerate() {
            if lsn > snapshot {
                break;
            }
            self.reads.fetch_add(1, Ordering::Relaxed);
            let frame = self
                .tape
                .read(lsn)?
                .ok_or_else(|| EngineError::rejected(ErrorKind::CorruptIndex))?;
            let decoded = decode(&frame.payload)
                .map_err(|_| EngineError::rejected(ErrorKind::CorruptIndex))?;
            if frame.header.kind != Kind::Turn
                || frame.header.tenant_id != tenant
                || decoded.agent_id != scope.agent
                || decoded.session_id != scope.session
                || decoded.seq != start + offset as u64
            {
                return Err(EngineError::rejected(ErrorKind::CorruptIndex));
            }
            out.push(Turn {
                lsn,
                seq: decoded.seq,
                commit_time_ns: frame.header.commit_time_ns,
                role: decoded.turn.role,
                content: decoded.turn.content,
                meta: decoded.turn.meta,
            });
        }
        Ok(out)
    }
    /// Waits for the checkpoint worker to install a snapshot. Never runs checkpoint I/O on the writer.
    pub fn checkpoint(&self) -> Result<u64> {
        self.worker.flush()
    }

    /// Returns published memory, checkpoint, and query counters.
    pub fn metrics(&self) -> Result<Metrics> {
        let tape_index = self.tape.index_stats()?;
        let state = lock(&self.state)?;
        Ok(Metrics {
            durable_lsn: self.tape.durable_lsn(),
            tape_index,
            hot_page_bytes: state.index.hot_bytes,
            dedup_entries: state.dedup.entries.len(),
            dedup_window_effective: state.dedup.effective_window(),
            checkpoint_watermark: state.watermark,
            checkpoint_files: state.chain.len(),
            checkpoint_saves: state.checkpoint_saves,
            checkpoint_buffer_peak_bytes: state.checkpoint_buffer_peak,
            checkpoint_pending_bytes: state.bytes.saturating_sub(state.checkpoint_bytes),
            writer_groups: self.tape.group_stats().0,
            writer_group_requests: self.tape.group_stats().1,
            superblock_installs: self.tape.superblock_installs(),
            replayed_records: state.replayed,
            checkpoint_warnings: state.warnings,
            checkpoint_snapshot_lock_ns: state.snapshot_lock_ns,
            checkpoint_state_lock_ns: state.checkpoint_lock_ns,
            window_reads: self.reads.load(Ordering::Relaxed),
            tenants: state.tenants.entries.len(),
        })
    }
}

/// One queued simulation append. Finish it only after stepping its engine.
#[cfg(feature = "sim")]
pub struct SimAppend<'a> {
    engine: &'a Engine,
    _pin: Pin<'a>,
    count: u64,
    reply: std::sync::mpsc::Receiver<zerodb_tape::WriteResult>,
}
#[cfg(feature = "sim")]
impl std::fmt::Debug for SimAppend<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimAppend").field("count", &self.count).finish_non_exhaustive()
    }
}
#[cfg(feature = "sim")]
impl SimAppend<'_> {
    /// Receives the post-publication receipt without reading the tape.
    pub fn finish(self) -> Result<Appended> {
        let reply = self
            .reply
            .recv()
            .map_err(|_| EngineError {
                kind: ErrorKind::Poisoned,
                outcome: zerodb_tape::Outcome::Unknown,
            })?
            .map_err(EngineError::write)?;
        match reply {
            Committed::Durable { txn, last_lsn, reply, .. } => {
                self.engine.receipt(self.count, txn, last_lsn, &reply)
            }
        }
    }
}

struct Pin<'a> {
    engine: &'a Engine,
    key: Key,
    active: bool,
}
impl Drop for Pin<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.engine.state.lock() {
            if let Some(entry) = state.index.sessions.get_mut(&self.key) {
                entry.pins = entry.pins.saturating_sub(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use zerodb_tape::sim::SimClock;
    use zerodb_tape::{CommittedRecord, SimDisk, StateMachine, TapeOptions};

    #[test]
    fn health_never_waits_and_failures_override_read_only() {
        enum Failure {
            None,
            Publication,
            Lock,
        }
        for (case, read_only, failure, expected) in [
            ("healthy", false, Failure::None, Health::Ok),
            ("read-only", true, Failure::None, Health::ReadOnly),
            ("publication failure beats read-only", true, Failure::Publication, Health::Poisoned),
            ("poisoned lock beats read-only", true, Failure::Lock, Health::Poisoned),
        ] {
            let io = Arc::new(SimDisk::new(0));
            let opts = EngineOptions {
                tape: TapeOptions {
                    segment_size: 64 * 1024,
                    free_space_floor: Some(4096),
                    ..TapeOptions::default()
                },
                checkpoint_interval: Duration::from_secs(3600),
                checkpoint_bytes: u64::MAX,
                ..EngineOptions::default()
            };
            let engine =
                Engine::open_with(io.clone(), Arc::new(SimClock::default()), "db", opts.clone())
                    .unwrap();
            if read_only {
                io.set_capacity(0).unwrap();
                let error = engine
                    .append(
                        &Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() },
                        vec![NewTurn { role: Role::User, content: vec![1], meta: vec![] }],
                        None,
                    )
                    .unwrap_err();
                assert_eq!(error.outcome, zerodb_tape::Outcome::NotCommitted, "{case}");
            }
            match failure {
                Failure::None => {}
                Failure::Publication => {
                    let mut machine =
                        Machine::new(engine.state.clone(), io, "db".into(), opts).unwrap();
                    machine.publish(&[CommittedRecord {
                        lsn: 1,
                        txn: 1,
                        kind: Kind::Fact,
                        tenant_id: 1,
                        commit_time_ns: 1,
                        payload: Arc::from([]),
                    }]);
                    assert!(engine.state.lock().unwrap().failure.is_some(), "{case}");
                }
                Failure::Lock => {
                    assert!(
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _state = engine.state.lock().unwrap();
                            panic!("injected state lock failure");
                        }))
                        .is_err(),
                        "{case}"
                    );
                }
            }
            assert_eq!(engine.tape.read_only(), read_only, "{case}");
            assert!(!engine.tape.is_poisoned(), "{case}");
            assert_eq!(engine.health(), expected, "{case}");
            if !matches!(failure, Failure::Lock) {
                std::thread::scope(|threads| {
                    let state = engine.state.lock().unwrap();
                    let (sender, receiver) = mpsc::sync_channel(1);
                    let engine = &engine;
                    threads.spawn(move || sender.send(engine.health()).unwrap());
                    let observed = receiver.recv_timeout(Duration::from_secs(5));
                    drop(state);
                    assert_eq!(observed, Ok(expected), "{case}: state lock held");
                });
            }
        }
    }
}
