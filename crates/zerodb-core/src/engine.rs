use crate::checkpoint::{self, Worker};
use crate::error::{lock, Result};
use crate::machine::Machine;
use crate::session::{address, Key};
use crate::state::State;
use crate::turn::{decode, encode};
use crate::{EngineError, EngineOptions, ErrorKind, NewTurn, Role, Trust};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Asserted trust, without filtering.
    pub trust: Trust,
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
/// Durable session storage. Drop joins the checkpoint worker before closing the tape.
pub struct Engine {
    worker: Worker,
    tape: Tape<Machine>,
    state: Arc<Mutex<State>>,
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
    /// Creates the directory, recovers its tape, and starts both workers using real filesystem I/O.
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
            *lock(&state)? = fresh;
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
        Ok(Self {
            worker,
            tape,
            state,
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
        let mut tenant = lock(&self.state)?.tenants.get(&scope.tenant);
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
                    return (|| {
                        if let Some(error) = &lock(&self.state)?.failure {
                            return Err(error.clone());
                        }
                        let seq = u64::from_le_bytes(
                            reply
                                .as_slice()
                                .try_into()
                                .map_err(|_| EngineError::rejected(ErrorKind::CorruptIndex))?,
                        );
                        let count = turns.len() as u64;
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
                    });
                }
                Ok(Committed::Fast) => unreachable!("turns always use durable acknowledgement"),
            }
        }
        Err(EngineError::rejected(ErrorKind::ResourceExhausted("hot_pages")))
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
            let decoded = decode(
                frame
                    .payload
                    .get(16..)
                    .ok_or_else(|| EngineError::rejected(ErrorKind::CorruptIndex))?,
            )
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
                trust: decoded.turn.trust,
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
        let state = lock(&self.state)?;
        Ok(Metrics {
            durable_lsn: self.tape.durable_lsn(),
            hot_page_bytes: state.index.hot_bytes,
            dedup_entries: state.dedup.entries.len(),
            dedup_window_effective: state.dedup.effective_window(),
            checkpoint_watermark: state.watermark,
            checkpoint_files: state.chain.len(),
            checkpoint_saves: state.checkpoint_saves,
            replayed_records: state.replayed,
            checkpoint_warnings: state.warnings,
            checkpoint_snapshot_lock_ns: state.snapshot_lock_ns,
            checkpoint_state_lock_ns: state.checkpoint_lock_ns,
            window_reads: self.reads.load(Ordering::Relaxed),
            tenants: state.tenants.entries.len(),
        })
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
