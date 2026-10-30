use crate::checkpoint::{self, Worker};
use crate::dedup::{self, digest, DedupKey};
use crate::error::{lock, Result};
use crate::fact::Fact;
use crate::facts::{visible, Rev};
use crate::machine::{Machine, REPLY_DEDUPLICATED};
use crate::session::{address, Key};
use crate::state::State;
use crate::turn::{decode, encode};
use crate::{EngineError, EngineOptions, ErrorKind, NewTurn, Role};
use atticdb_tape::{
    Clock, Committed, Digest, Durability, Frame, Header, Io, Kind, RecordInput, StdIo, SystemClock,
    Tape,
};
use std::collections::BinaryHeap;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

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
    /// True when a retry key returned an earlier receipt and nothing new was written.
    pub deduplicated: bool,
}
/// A fact revision to store under a tenant-scoped key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewFact {
    /// UTF-8 key, 1 to 1024 bytes. Hierarchy is addressing only.
    pub key: String,
    /// Opaque content, at most 4 MiB.
    pub content: Vec<u8>,
    /// Opaque metadata, at most 64 KiB.
    pub meta: Vec<u8>,
    /// Attribution only, at most 255 bytes. It does not change the key.
    pub agent: String,
}
/// A durable fact write receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactWritten {
    /// Original transaction, including on retries.
    pub txn: u64,
    /// The revision's LSN. A later conditional write passes it as `if_lsn`.
    pub lsn: u64,
    /// True when a retry key returned an earlier receipt and nothing new was written.
    pub deduplicated: bool,
}
/// A fact's current value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredFact {
    /// Tenant-scoped key.
    pub key: String,
    /// The revision's LSN, usable as `if_lsn`.
    pub lsn: u64,
    /// Commit timestamp from the tape.
    pub commit_time_ns: u64,
    /// Attribution of this revision.
    pub agent: String,
    /// Opaque content.
    pub content: Vec<u8>,
    /// Opaque metadata.
    pub meta: Vec<u8>,
}
/// One revision of a fact key. A deleted revision has empty content and meta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Revision {
    /// The revision's LSN.
    pub lsn: u64,
    /// Commit timestamp from the tape.
    pub commit_time_ns: u64,
    /// Attribution of this revision.
    pub agent: String,
    /// True for a tombstone.
    pub deleted: bool,
    /// Opaque content.
    pub content: Vec<u8>,
    /// Opaque metadata.
    pub meta: Vec<u8>,
}
/// A key's revisions, newest first, at most `fact_history`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FactHistory {
    /// Whole revisions, newest first.
    pub revisions: Vec<Revision>,
    /// True when older revisions exist than the ones returned.
    pub truncated: bool,
}
/// A key-ordered listing of current facts under a byte prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactQuery {
    /// Byte prefix. End it with `/` for path semantics.
    pub prefix: String,
    /// Exclusive cursor: a previous page's `next`.
    pub after: Option<String>,
    /// Maximum facts per page, at least 1.
    pub limit: usize,
    /// Key, agent, content and meta bytes per page. The first fact is always returned.
    pub max_bytes: usize,
}
/// One page of current facts in key order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FactPage {
    /// Current facts. Deleted keys are skipped.
    pub facts: Vec<StoredFact>,
    /// The last key the walk finished with, returned or skipped. None once it left the prefix.
    pub next: Option<String>,
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
/// A search: every word of `query` must appear, ignoring case, in a record.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchQuery {
    /// Words separated by whitespace. Matched as substrings, so `deploy` finds `deployment`.
    /// A plural word also matches its singular, so `keys` finds `key`.
    pub query: String,
    /// Exclusive cursor: only records below this LSN. A previous page's `next`.
    pub before: Option<u64>,
    /// Maximum hits per page, at least 1.
    pub limit: usize,
    /// Content, meta and key bytes per page. The first hit is always returned.
    pub max_bytes: usize,
    /// Records to visit at most, which bounds the call's cost. `next` resumes below them.
    pub visits: usize,
}
/// One search result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Hit {
    /// A turn and the session it belongs to.
    Turn {
        /// Agent identifier.
        agent: String,
        /// Session identifier.
        session: String,
        /// The turn.
        turn: Turn,
    },
    /// A fact's current value.
    Fact(StoredFact),
}
/// One page of hits, newest first.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchPage {
    /// Matching turns and current facts, newest first.
    pub hits: Vec<Hit>,
    /// Pass as `before` to continue. A page can be empty while it is set. None once the walk
    /// reached the oldest record.
    pub next: Option<u64>,
}
/// A listing of a tenant's sessions in byte order of agent, then session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionQuery {
    /// Exclusive cursor as (agent, session): a previous page's `next`.
    pub after: Option<(String, String)>,
    /// Maximum sessions per page, at least 1.
    pub limit: usize,
}
/// A session with at least 1 published turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInfo {
    /// Agent identifier.
    pub agent: String,
    /// Session identifier.
    pub session: String,
    /// Published turns. Sequences are dense, so this is also the newest turn's sequence.
    pub turns: u64,
}
/// One page of sessions, with totals over all of the tenant's sessions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionPage {
    /// Sessions in byte order of agent, then session.
    pub sessions: Vec<SessionInfo>,
    /// The page's last session as (agent, session) when more follow. Pass it as `after`.
    pub next: Option<(String, String)>,
    /// The tenant's sessions with a published turn, not only this page's.
    pub total_sessions: usize,
    /// The tenant's published turns, not only this page's.
    pub total_turns: u64,
}
/// Which records a [`RecordQuery`] returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordKind {
    /// Turns only.
    Turn,
    /// Fact revisions only, deletes included.
    Fact,
}
/// A newest-first walk over a tenant's turns and fact revisions, each as written.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordQuery {
    /// Exclusive cursor: only records below this LSN. A previous page's `next`.
    pub before: Option<u64>,
    /// Maximum records per page, at least 1.
    pub limit: usize,
    /// Only this kind. None returns both.
    pub kind: Option<RecordKind>,
    /// Agent, session, key, content and meta bytes per page. The first record is always
    /// returned.
    pub max_bytes: usize,
    /// Records to visit at most, which bounds the call's cost. Other tenants' records and
    /// skipped kinds count too. `next` resumes below them.
    pub visits: usize,
}
/// One record as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A turn and the session it belongs to.
    Turn {
        /// Agent identifier.
        agent: String,
        /// Session identifier.
        session: String,
        /// The turn.
        turn: Turn,
    },
    /// A fact revision, current or not. A delete has `deleted` set and empty content and meta.
    Fact {
        /// Tenant-scoped key.
        key: String,
        /// The revision.
        revision: Revision,
    },
}
/// One page of records, newest first.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordPage {
    /// Turns and fact revisions, newest first.
    pub records: Vec<Record>,
    /// Pass as `before` to continue. A page can be empty while it is set. None once the walk
    /// reached the oldest record.
    pub next: Option<u64>,
}
/// A session window cut by a byte budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Window {
    /// Whole turns in sequence order.
    pub turns: Vec<Turn>,
    /// True when the budget stopped the window before the query's own bound.
    pub truncated: bool,
}
/// Observable bounds and recovery counters.
#[derive(Clone, Debug)]
pub struct Metrics {
    /// Highest readable tape LSN.
    pub durable_lsn: u64,
    /// Tape-index resident memory, reloads and eviction counters.
    pub tape_index: atticdb_tape::LsnIndexStats,
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
    /// Writer time per stage, summed over committed groups during this open.
    pub writer_stages: atticdb_tape::StageTimes,
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
    /// Tape::read calls issued by windows, excluding append receipts.
    /// A budget-cut window adds 1 read for the turn it left out.
    pub window_reads: u64,
    /// Registered tenants.
    pub tenants: usize,
    /// Tape::read calls issued by fact reads. A budget-cut call adds 1 read for the entry it
    /// left out.
    pub fact_reads: u64,
    /// Fact keys held, tombstones included.
    pub facts: usize,
    /// Revision pointers held, at most `fact_history + 2` per key.
    pub fact_revisions: usize,
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
    fact_reads: AtomicU64,
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
        std::fs::create_dir_all(path.as_ref()).map_err(atticdb_tape::Error::from)?;
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
            || opts.max_facts == 0
            || !(1..=4096).contains(&opts.fact_history)
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
            eprintln!("atticdb: checkpoint inconsistent with recovered tape, replaying tape");
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
            fact_reads: AtomicU64::new(0),
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
        let encode_all = || {
            turns
                .iter()
                .map(|turn| encode(&scope.agent, &scope.session, 0, turn, idem_key))
                .collect::<std::result::Result<Vec<_>, _>>()
        };
        let payloads = encode_all()?;
        let digest = idem_key.map(|_| digest(payloads.iter().map(Vec::as_slice)));
        let retry = idem_key
            .zip(digest)
            .map(|(id, digest)| (dedup::Scope::Session(scope.session.clone()), id, digest));
        let (tenant, _creation) = match self.start(&scope.tenant, retry)? {
            Start::Cached(Committed::Durable { txn, last_lsn, reply, .. }) => {
                return self.receipt(turns.len() as u64, txn, last_lsn, &reply, true);
            }
            Start::Write(tenant, creation) => (tenant, creation),
        };
        let records = |payloads: Vec<Vec<u8>>| {
            let mut records = Vec::with_capacity(payloads.len() + 1);
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
            records
        };
        // The first write moves the payloads. A hot-page retry re-encodes the same inputs, which
        // yields the same bytes the digest covers, so the common path never copies a payload.
        let mut payloads = Some(payloads);
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
            let payloads = match payloads.take() {
                Some(payloads) => payloads,
                None => encode_all()?,
            };
            let result = self
                .tape
                .append_with_digest(records(payloads), Durability::Durable, digest, None)
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
                    return self.receipt(turns.len() as u64, txn, last_lsn, &reply, false);
                }
            }
        }
        Err(EngineError::rejected(ErrorKind::ResourceExhausted("hot_pages")))
    }

    /// Answers a covered retry from published state, else resolves the tenant.
    /// An unknown tenant holds the creation guard until the write returns.
    fn start(
        &self,
        tenant: &str,
        retry: Option<(dedup::Scope, &str, Digest)>,
    ) -> Result<Start<'_>> {
        {
            let state = lock(&self.state)?;
            let id = state.tenants.get(tenant);
            if let (Some(id), Some((scope, idem_key, digest))) = (id, retry) {
                if let Some(entry) = state.dedup.get(&DedupKey(id, scope, idem_key.to_owned())) {
                    if let Some(error) = &state.failure {
                        return Err(error.clone());
                    }
                    let committed = entry.resolve(digest)?;
                    let Committed::Durable { last_lsn, .. } = committed;
                    // Read the tape watermark after taking the state lock, like window_snapshot.
                    // Machine publication releases this lock before the tape advances its watermark.
                    // Only covered receipts bypass the writer, so the next read cannot lag this ack.
                    if last_lsn <= self.tape.durable_lsn() {
                        return Ok(Start::Cached(committed));
                    }
                }
            }
            if id.is_some() {
                return Ok(Start::Write(id, None));
            }
        }
        let guard = lock(&self.tenant_creation)?;
        Ok(Start::Write(lock(&self.state)?.tenants.get(tenant), Some(guard)))
    }

    /// Stores a fact revision. It supersedes the key's current value by LSN.
    ///
    /// `if_lsn` makes the write conditional on the key's current LSN, 0 meaning no current
    /// value. A mismatch is `Conflict`. A retry key returns the original receipt before the
    /// condition is checked, so `if_lsn` is not part of a retry's identity.
    pub fn fact_put(
        &self,
        tenant: &str,
        fact: NewFact,
        idem_key: Option<&str>,
        if_lsn: Option<u64>,
    ) -> Result<FactWritten> {
        self.write_fact(tenant, fact, false, idem_key, if_lsn)
    }

    /// Writes a tombstone. The key then has no current value. A tombstone keeps its key slot.
    pub fn fact_delete(
        &self,
        tenant: &str,
        key: &str,
        agent: &str,
        idem_key: Option<&str>,
        if_lsn: Option<u64>,
    ) -> Result<FactWritten> {
        let fact = NewFact { key: key.into(), content: vec![], meta: vec![], agent: agent.into() };
        self.write_fact(tenant, fact, true, idem_key, if_lsn)
    }

    fn write_fact(
        &self,
        tenant: &str,
        fact: NewFact,
        tombstone: bool,
        idem_key: Option<&str>,
        if_lsn: Option<u64>,
    ) -> Result<FactWritten> {
        if tenant.len() > 255 {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let (key, payload, digest) = fact_payload(fact, tombstone, idem_key)?;
        let retry = idem_key.zip(digest).map(|(id, digest)| (dedup::Scope::Fact(key), id, digest));
        let (committed, cached) = match self.start(tenant, retry)? {
            Start::Cached(committed) => (committed, true),
            Start::Write(id, _creation) => {
                let mut records = Vec::with_capacity(2);
                if id.is_none() {
                    records.push(RecordInput {
                        kind: Kind::TenantCreate,
                        tenant_id: 0,
                        content: tenant.as_bytes().to_vec(),
                    });
                }
                records.push(RecordInput {
                    kind: Kind::Fact,
                    tenant_id: id.unwrap_or(0),
                    content: payload,
                });
                let committed = self
                    .tape
                    .append_with_digest(records, Durability::Durable, digest, if_lsn)
                    .map_err(EngineError::write)?;
                (committed, false)
            }
        };
        self.fact_written(committed, cached)
    }

    fn fact_written(&self, committed: Committed, cached: bool) -> Result<FactWritten> {
        let Committed::Durable { txn, last_lsn, reply, .. } = committed;
        let deduplicated = cached || reply == [REPLY_DEDUPLICATED];
        self.published(|| Ok(FactWritten { txn, lsn: last_lsn, deduplicated }))
    }

    /// Queues a fact write for a later inline step. Register its tenant first.
    #[cfg(feature = "sim")]
    pub fn enqueue_fact_sim(
        &self,
        tenant: &str,
        fact: NewFact,
        tombstone: bool,
        idem_key: Option<&str>,
        if_lsn: Option<u64>,
    ) -> Result<SimFact<'_>> {
        let id = lock(&self.state)?
            .tenants
            .get(tenant)
            .ok_or_else(|| EngineError::rejected(ErrorKind::InvalidInput))?;
        let (_, content, digest) = fact_payload(fact, tombstone, idem_key)?;
        let records = vec![RecordInput { kind: Kind::Fact, tenant_id: id, content }];
        Ok(SimFact { engine: self, reply: self.tape.enqueue_sim(records, digest, if_lsn)? })
    }

    /// Runs a receipt builder after the writer acknowledged a commit. Publication failures
    /// and receipt errors report `Committed`.
    fn published<T>(&self, build: impl FnOnce() -> Result<T>) -> Result<T> {
        (|| {
            if let Some(error) = &lock(&self.state)?.failure {
                return Err(error.clone());
            }
            build()
        })()
        .map_err(|mut error: EngineError| {
            error.outcome = atticdb_tape::Outcome::Committed;
            error
        })
    }

    /// Builds a receipt from a writer reply: the 8-byte first sequence, plus the
    /// cached-reply marker when preparation answered from the dedup table.
    fn receipt(
        &self,
        count: u64,
        txn: u64,
        last_lsn: u64,
        reply: &[u8],
        cached: bool,
    ) -> Result<Appended> {
        self.published(|| {
            let (seq, marked) = match reply {
                [seq @ .., REPLY_DEDUPLICATED] if seq.len() == 8 => (seq, true),
                seq => (seq, false),
            };
            let seq = u64::from_le_bytes(
                seq.try_into().map_err(|_| EngineError::rejected(ErrorKind::CorruptIndex))?,
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
                deduplicated: cached || marked,
            })
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
        let digest = idem_key.map(|_| digest(records.iter().map(|r| r.content.as_slice())));
        let key = Key(tenant, scope.agent.clone(), scope.session.clone());
        let pin = match self.pin(key.clone()) {
            Err(EngineError { kind: ErrorKind::ResourceExhausted("hot_pages"), .. }) => {
                self.worker.flush()?;
                self.pin(key)?
            }
            result => result?,
        };
        let reply = self.tape.enqueue_sim(records, digest, None)?;
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
        self.window_budget(scope, query, usize::MAX).map(|window| window.turns)
    }

    /// Reads a window whose turns' `content` and `meta` bytes total at most `max_bytes`.
    ///
    /// `Last` keeps the newest turns and `Since` the oldest. Turns are never cut, so a first
    /// turn over budget returns no turns. Tape reads are at most returned turns plus 1.
    pub fn window_budget(
        &self,
        scope: &Scope,
        query: WindowQuery,
        max_bytes: usize,
    ) -> Result<Window> {
        match self.window_snapshot(scope, query, max_bytes) {
            Err(EngineError { kind: ErrorKind::ResourceExhausted("hot_pages"), .. }) => {
                self.worker.flush()?;
                self.window_snapshot(scope, query, max_bytes)
            }
            result => result,
        }
    }
    fn window_snapshot(
        &self,
        scope: &Scope,
        query: WindowQuery,
        max_bytes: usize,
    ) -> Result<Window> {
        let empty = Window { turns: Vec::new(), truncated: false };
        let state = lock(&self.state)?;
        let snapshot = self.tape.durable_lsn();
        let Some(tenant) = state.tenants.get(&scope.tenant) else {
            return Ok(empty);
        };
        let key = Key(tenant, scope.agent.clone(), scope.session.clone());
        let Some(entry) = state.index.sessions.get(&key) else {
            return Ok(empty);
        };
        let end = entry.len.saturating_add(1);
        let (start, end) = match query {
            WindowQuery::Last(n) => (end.saturating_sub(n as u64).max(1), end),
            WindowQuery::Since { seq, limit } => {
                (seq.max(1).min(end), end.min(seq.max(1).saturating_add(limit as u64)))
            }
        };
        drop(state);
        // Lazy, in query order: a budget stop loads no further pages and reads no more turns.
        let seqs: Box<dyn Iterator<Item = u64>> = match query {
            WindowQuery::Last(_) => Box::new((start..end).rev()),
            WindowQuery::Since { .. } => Box::new(start..end),
        };
        let mut turns = Vec::with_capacity((end - start).min(64) as usize);
        let mut bytes = 0usize;
        let mut truncated = false;
        let mut current: Option<(usize, Arc<Vec<u64>>)> = None;
        let mut reader = self.tape.reader();
        for seq in seqs {
            let (page, offset) = address(seq);
            if current.as_ref().is_none_or(|(loaded, _)| *loaded != page) {
                current = Some((page, self.load_page(&key, page)?));
            }
            let lsn = *current
                .as_ref()
                .and_then(|(_, slots)| slots.get(offset))
                .ok_or_else(|| EngineError::rejected(ErrorKind::CorruptIndex))?;
            // Session LSNs grow with seq, so turns past the snapshot form a suffix.
            if lsn > snapshot {
                match query {
                    WindowQuery::Last(_) => continue,
                    WindowQuery::Since { .. } => break,
                }
            }
            self.reads.fetch_add(1, Ordering::Relaxed);
            let frame =
                reader.read(lsn)?.ok_or_else(|| EngineError::rejected(ErrorKind::CorruptIndex))?;
            let decoded = decode(&frame.payload)
                .map_err(|_| EngineError::rejected(ErrorKind::CorruptIndex))?;
            if frame.header.kind != Kind::Turn
                || frame.header.tenant_id != tenant
                || decoded.agent_id != scope.agent
                || decoded.session_id != scope.session
                || decoded.seq != seq
            {
                return Err(EngineError::rejected(ErrorKind::CorruptIndex));
            }
            let size = decoded.turn.content.len().saturating_add(decoded.turn.meta.len());
            match bytes.checked_add(size).filter(|&total| total <= max_bytes) {
                Some(total) => bytes = total,
                None => {
                    truncated = true;
                    break;
                }
            }
            turns.push(Turn {
                lsn,
                seq: decoded.seq,
                commit_time_ns: frame.header.commit_time_ns,
                role: decoded.turn.role,
                content: decoded.turn.content,
                meta: decoded.turn.meta,
            });
        }
        if matches!(query, WindowQuery::Last(_)) {
            turns.reverse();
        }
        Ok(Window { turns, truncated })
    }
    /// Returns a key's current value at a durable snapshot, or None when it has none.
    pub fn fact_get(&self, tenant: &str, key: &str) -> Result<Option<StoredFact>> {
        let (id, rev) = {
            let Some((state, snapshot, id)) = self.fact_snapshot(tenant)? else {
                return Ok(None);
            };
            let revs = state.facts.map.get(&(id, key.to_owned()));
            match revs.and_then(|revs| visible(revs, snapshot).last()).filter(|rev| !rev.deleted) {
                Some(rev) => (id, *rev),
                None => return Ok(None),
            }
        };
        let (mut read, _) = self.read_facts(id, [(key, rev)], usize::MAX)?;
        Ok(read.pop().map(stored))
    }

    /// Returns up to `fact_history` revisions, newest first, within `max_bytes` but at least one.
    pub fn fact_history(&self, tenant: &str, key: &str, max_bytes: usize) -> Result<FactHistory> {
        let (id, newest, truncated) = {
            let Some((state, snapshot, id)) = self.fact_snapshot(tenant)? else {
                return Ok(FactHistory::default());
            };
            let Some(revs) = state.facts.map.get(&(id, key.to_owned())) else {
                return Ok(FactHistory::default());
            };
            let (shown, history) = (visible(revs, snapshot), state.facts.history);
            let truncated = shown.len() > history || (revs.dropped && !shown.is_empty());
            (id, shown.iter().rev().take(history).copied().collect::<Vec<_>>(), truncated)
        };
        let newest = newest.into_iter().map(|rev| (key, rev));
        let (read, cut) = self.read_facts(id, newest, max_bytes)?;
        let revisions = read
            .into_iter()
            .map(|(rev, commit_time_ns, fact)| Revision {
                lsn: rev.lsn,
                commit_time_ns,
                agent: fact.agent,
                deleted: rev.deleted,
                content: fact.content,
                meta: fact.meta,
            })
            .collect();
        Ok(FactHistory { revisions, truncated: cut || truncated })
    }

    /// Lists current facts under a prefix in key order, visiting at most 1,024 keys per call.
    pub fn fact_list(&self, tenant: &str, query: FactQuery) -> Result<FactPage> {
        if query.limit == 0 {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let (id, picked, mut next) = {
            let Some((state, snapshot, id)) = self.fact_snapshot(tenant)? else {
                return Ok(FactPage::default());
            };
            let start = match &query.after {
                Some(after) if *after >= query.prefix => Bound::Excluded((id, after.clone())),
                _ => Bound::Included((id, query.prefix.clone())),
            };
            let (mut picked, mut next, mut last, mut visited) =
                (Vec::new(), None, None::<&String>, 0);
            for ((owner, key), revs) in state.facts.map.range((start, Bound::Unbounded)) {
                if *owner != id || !key.starts_with(&query.prefix) {
                    break;
                }
                if picked.len() == query.limit || visited == 1024 {
                    next = last.cloned();
                    break;
                }
                visited += 1;
                last = Some(key);
                if let Some(rev) = visible(revs, snapshot).last().filter(|rev| !rev.deleted) {
                    picked.push((key.clone(), *rev));
                }
            }
            (id, picked, next)
        };
        let pairs = picked.iter().map(|(key, rev)| (key.as_str(), *rev));
        let (read, cut) = self.read_facts(id, pairs, query.max_bytes)?;
        let facts: Vec<_> = read.into_iter().map(stored).collect();
        if cut {
            next = facts.last().map(|fact| fact.key.clone());
        }
        Ok(FactPage { facts, next })
    }

    /// Walks records newest first and returns turns and current facts that contain every
    /// word of the query. Visits at most `query.visits` records; `next` resumes below them.
    pub fn search(&self, tenant: &str, query: SearchQuery) -> Result<SearchPage> {
        let words: Vec<String> = query.query.split_whitespace().map(str::to_lowercase).collect();
        let stems: Vec<Option<String>> = words.iter().map(|word| singular(word)).collect();
        if words.is_empty() {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        // A fact counts only while it is its key's newest durable revision and not deleted,
        // checked when the walk reaches it. So a superseded value never shows. The index keeps
        // old revisions only for readers at the previous durable LSN, so a key changed during
        // the walk may be missing from this call, like any record written after it started.
        let current = |id: u32, key: &str, lsn: u64| -> Result<bool> {
            let state = lock(&self.state)?;
            let revs = state.facts.map.get(&(id, key.to_owned()));
            let newest = revs.and_then(|revs| visible(revs, self.tape.durable_lsn()).last());
            Ok(newest.is_some_and(|rev| rev.lsn == lsn && !rev.deleted))
        };
        let found = |fields: &[&[u8]]| {
            let text: Vec<String> =
                fields.iter().map(|field| String::from_utf8_lossy(field).to_lowercase()).collect();
            let has = |word: &str| text.iter().any(|field| field.contains(word));
            words
                .iter()
                .zip(&stems)
                .all(|(word, stem)| has(word) || stem.as_deref().is_some_and(has))
        };
        let walk = Walk {
            before: query.before,
            limit: query.limit,
            max_bytes: query.max_bytes,
            visits: query.visits,
        };
        let wanted = |header: &Header| matches!(header.kind, Kind::Turn | Kind::Fact);
        let (hits, next) = self.newest_first(tenant, walk, wanted, |frame| {
            let hit = match frame.header.kind {
                Kind::Turn => {
                    let (agent, session, turn) = turn_frame(frame)?;
                    if !found(&[&turn.content]) {
                        return Ok(None);
                    }
                    Hit::Turn { agent, session, turn }
                }
                Kind::Fact => {
                    let corrupt = |_| EngineError::rejected(ErrorKind::CorruptIndex);
                    let fact = crate::fact::decode(&frame.payload).map_err(corrupt)?;
                    let (id, lsn) = (frame.header.tenant_id, frame.header.lsn);
                    let keep = found(&[fact.key.as_bytes(), &fact.content])
                        && current(id, &fact.key, lsn)?;
                    if !keep {
                        return Ok(None);
                    }
                    let rev = Rev { lsn, deleted: false };
                    Hit::Fact(stored((rev, frame.header.commit_time_ns, fact)))
                }
                _ => return Ok(None),
            };
            let size = match &hit {
                Hit::Turn { turn, .. } => turn.content.len() + turn.meta.len(),
                Hit::Fact(fact) => fact.key.len() + fact.content.len() + fact.meta.len(),
            };
            Ok(Some((hit, size)))
        })?;
        Ok(SearchPage { hits, next })
    }

    /// Walks a tenant's turns and fact revisions newest first, each as written. Superseded
    /// revisions and deletes show too: it is the log, not the current state. Visits at most
    /// `query.visits` records; `next` resumes below them.
    pub fn records(&self, tenant: &str, query: RecordQuery) -> Result<RecordPage> {
        // Forgets are not applied yet. Once they are, hide here what `window` and `search` hide.
        let walk = Walk {
            before: query.before,
            limit: query.limit,
            max_bytes: query.max_bytes,
            visits: query.visits,
        };
        let wanted = |header: &Header| match query.kind {
            None => matches!(header.kind, Kind::Turn | Kind::Fact),
            Some(RecordKind::Turn) => header.kind == Kind::Turn,
            Some(RecordKind::Fact) => header.kind == Kind::Fact,
        };
        let (records, next) = self.newest_first(tenant, walk, wanted, |frame| {
            let record = match frame.header.kind {
                Kind::Turn => {
                    let (agent, session, turn) = turn_frame(frame)?;
                    Record::Turn { agent, session, turn }
                }
                Kind::Fact => {
                    let corrupt = |_| EngineError::rejected(ErrorKind::CorruptIndex);
                    let fact = crate::fact::decode(&frame.payload).map_err(corrupt)?;
                    let Fact { key, content, meta, agent, tombstone, .. } = fact;
                    let (lsn, commit_time_ns) = (frame.header.lsn, frame.header.commit_time_ns);
                    let deleted = tombstone;
                    let revision = Revision { lsn, commit_time_ns, agent, deleted, content, meta };
                    Record::Fact { key, revision }
                }
                _ => return Ok(None),
            };
            // Every variable-length field counts, so `max_bytes` bounds all of a page's
            // variable bytes, not only content and meta.
            let size = match &record {
                Record::Turn { agent, session, turn } => {
                    agent.len() + session.len() + turn.content.len() + turn.meta.len()
                }
                Record::Fact { key, revision: rev } => {
                    key.len() + rev.agent.len() + rev.content.len() + rev.meta.len()
                }
            };
            Ok(Some((record, size)))
        })?;
        Ok(RecordPage { records, next })
    }

    /// The newest-first walk `search` and `records` share, so the two cannot drift. It reads
    /// the tenant's records below `before` at the snapshot other readers use.
    ///
    /// `wanted` filters frames by header. A frame of another tenant, or one `wanted` rejects,
    /// costs a 40-byte header read and gets no CRC check. So a flipped bit in a wanted frame's
    /// tenant or kind can drop it from `records` and `search` with no error. `window`, the fact
    /// reads and `atticdb verify` read in full and still catch it. `pick` keeps a wanted record
    /// as an item with its size, or skips it.
    ///
    /// Every LSN the walk visits counts toward `visits`, other tenants' records and skipped
    /// kinds included. The first item is kept even when it alone passes `max_bytes`. `next`
    /// says where the next page starts:
    /// - after an item over `max_bytes`: its LSN + 1, so that item comes first;
    /// - after `limit` items: the last item's LSN;
    /// - after `visits` LSNs: the lowest LSN visited. So a page can be empty while `next` is set.
    ///
    /// The last 2 give None when no LSN is left below.
    fn newest_first<T>(
        &self,
        tenant: &str,
        walk: Walk,
        wanted: impl Fn(&Header) -> bool,
        mut pick: impl FnMut(&Frame) -> Result<Option<(T, usize)>>,
    ) -> Result<(Vec<T>, Option<u64>)> {
        if walk.limit == 0 || walk.visits == 0 {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let Some((state, snapshot, id)) = self.fact_snapshot(tenant)? else {
            return Ok((Vec::new(), None));
        };
        // Release the state lock before any tape read.
        drop(state);
        let top = walk.before.map_or(snapshot, |before| before.saturating_sub(1).min(snapshot));
        let (mut items, mut bytes, mut reader) = (Vec::new(), 0usize, self.tape.reader());
        for lsn in (1..=top).rev().take(walk.visits) {
            // A kept frame passes its CRC check, so its header is the one this filter saw.
            let mine = |header: &Header| header.tenant_id == id && wanted(header);
            let Some(frame) = reader.read_if(lsn, mine)? else { continue };
            let Some((item, size)) = pick(&frame)? else { continue };
            bytes = bytes.saturating_add(size);
            if bytes > walk.max_bytes && !items.is_empty() {
                return Ok((items, Some(lsn + 1)));
            }
            items.push(item);
            if items.len() == walk.limit {
                return Ok((items, Some(lsn).filter(|&lsn| lsn > 1)));
            }
        }
        let low = top.saturating_sub(walk.visits as u64);
        Ok((items, (low > 0).then_some(low + 1)))
    }

    /// Lists a tenant's sessions that have a published turn, in byte order of agent, then
    /// session, with totals over all of them.
    ///
    /// `turns` counts published turns. The writer publishes after the sync, so a counted turn
    /// is durable. `window`, `records` and `search` can read it a few microseconds after it is
    /// counted.
    ///
    /// It makes 1 pass over the session index under the state lock. A max-heap keeps the
    /// `limit` smallest keys above the cursor. That is O(sessions) time and O(limit) memory.
    /// The index holds every tenant's sessions, so the pass covers them all. Writers wait for
    /// this pass. A first page over 100,000 sessions holds the lock about 1 to 4 ms at limit
    /// 100 to 1,000. A page after a cursor costs about 1.5x that. At the default cap of
    /// 1,000,000 sessions, a first page holds it about 25 to 30 ms. All figures are from
    /// release mode on an Apple M4.
    pub fn sessions(&self, tenant: &str, query: SessionQuery) -> Result<SessionPage> {
        if query.limit == 0 {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let state = lock(&self.state)?;
        let Some(id) = state.tenants.get(tenant) else {
            return Ok(SessionPage::default());
        };
        let after = query.after.as_ref().map(|(agent, session)| (agent.as_str(), session.as_str()));
        let (mut page, mut smallest, mut more) = (SessionPage::default(), BinaryHeap::new(), false);
        for (Key(owner, agent, session), entry) in &state.index.sessions {
            // `len` counts published turns. A session with only reserved ones is skipped.
            if *owner != id || entry.len == 0 {
                continue;
            }
            page.total_sessions += 1;
            page.total_turns = page.total_turns.saturating_add(entry.len);
            let key = (agent.as_str(), session.as_str());
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            if smallest.len() < query.limit {
                smallest.push((key, entry.len));
                continue;
            }
            more = true;
            if let Some(mut largest) = smallest.peek_mut() {
                if key < largest.0 {
                    *largest = (key, entry.len);
                }
            }
        }
        let sessions = smallest.into_sorted_vec().into_iter();
        page.sessions = sessions
            .map(|((agent, session), turns)| SessionInfo {
                agent: agent.to_owned(),
                session: session.to_owned(),
                turns,
            })
            .collect();
        if more {
            page.next = page.sessions.last().map(|last| (last.agent.clone(), last.session.clone()));
        }
        Ok(page)
    }

    /// Takes the state lock, then the durable snapshot readers use, and resolves the tenant.
    fn fact_snapshot(&self, tenant: &str) -> Result<Option<(MutexGuard<'_, State>, u64, u32)>> {
        let state = lock(&self.state)?;
        let snapshot = self.tape.durable_lsn();
        Ok(state.tenants.get(tenant).map(|id| (state, snapshot, id)))
    }

    /// Reads frames until `max_bytes` would pass, keeping the first; counts the cut read too.
    fn read_facts<'k>(
        &self,
        tenant: u32,
        revs: impl IntoIterator<Item = (&'k str, Rev)>,
        max_bytes: usize,
    ) -> Result<(Vec<ReadFact>, bool)> {
        let corrupt = || EngineError::rejected(ErrorKind::CorruptIndex);
        let (mut read, mut bytes, mut reader) = (Vec::new(), 0usize, self.tape.reader());
        for (key, rev) in revs {
            self.fact_reads.fetch_add(1, Ordering::Relaxed);
            let frame = reader.read(rev.lsn)?.ok_or_else(corrupt)?;
            let fact = crate::fact::decode(&frame.payload).map_err(|_| corrupt())?;
            if frame.header.kind != Kind::Fact
                || frame.header.tenant_id != tenant
                || fact.key != key
                || fact.tombstone != rev.deleted
            {
                return Err(corrupt());
            }
            bytes = bytes.saturating_add(
                key.len() + fact.agent.len() + fact.content.len() + fact.meta.len(),
            );
            if bytes > max_bytes && !read.is_empty() {
                return Ok((read, true));
            }
            read.push((rev, frame.header.commit_time_ns, fact));
        }
        Ok((read, false))
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
            writer_stages: self.tape.stage_times(),
            superblock_installs: self.tape.superblock_installs(),
            replayed_records: state.replayed,
            checkpoint_warnings: state.warnings,
            checkpoint_snapshot_lock_ns: state.snapshot_lock_ns,
            checkpoint_state_lock_ns: state.checkpoint_lock_ns,
            window_reads: self.reads.load(Ordering::Relaxed),
            tenants: state.tenants.entries.len(),
            fact_reads: self.fact_reads.load(Ordering::Relaxed),
            facts: state.facts.map.len(),
            fact_revisions: state.facts.revisions,
        })
    }
}

/// One queued simulation append. Finish it only after stepping its engine.
#[cfg(feature = "sim")]
pub struct SimAppend<'a> {
    engine: &'a Engine,
    _pin: Pin<'a>,
    count: u64,
    reply: std::sync::mpsc::Receiver<atticdb_tape::WriteResult>,
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
                outcome: atticdb_tape::Outcome::Unknown,
            })?
            .map_err(EngineError::write)?;
        match reply {
            Committed::Durable { txn, last_lsn, reply, .. } => {
                self.engine.receipt(self.count, txn, last_lsn, &reply, false)
            }
        }
    }
}

enum Start<'a> {
    Cached(Committed),
    Write(Option<u32>, Option<std::sync::MutexGuard<'a, ()>>),
}

/// Encodes a fact payload and its retry digest. Returns the key for the retry scope.
fn fact_payload(
    fact: NewFact,
    tombstone: bool,
    idem_key: Option<&str>,
) -> Result<(String, Vec<u8>, Option<Digest>)> {
    let NewFact { key, content, meta, agent } = fact;
    let idem = idem_key.map(str::to_owned);
    let fact = Fact { key, content, meta, agent, source_refs: vec![], idem_key: idem, tombstone };
    let payload = crate::fact::encode(&fact)?;
    let digest = idem_key.map(|_| digest([payload.as_slice()]));
    Ok((fact.key, payload, digest))
}

/// One queued simulation fact write. Finish it only after stepping its engine.
#[cfg(feature = "sim")]
#[derive(Debug)]
pub struct SimFact<'a> {
    engine: &'a Engine,
    reply: std::sync::mpsc::Receiver<atticdb_tape::WriteResult>,
}
#[cfg(feature = "sim")]
impl SimFact<'_> {
    /// Receives the post-publication receipt without reading the tape.
    pub fn finish(self) -> Result<FactWritten> {
        let reply = self.reply.recv().map_err(|_| EngineError {
            kind: ErrorKind::Poisoned,
            outcome: atticdb_tape::Outcome::Unknown,
        })?;
        self.engine.fact_written(reply.map_err(EngineError::write)?, false)
    }
}

/// A fact frame read for a revision, with its commit time.
type ReadFact = (Rev, u64, Fact);

/// The bounds of a newest-first walk, from a search or a records query.
struct Walk {
    before: Option<u64>,
    limit: usize,
    max_bytes: usize,
    visits: usize,
}

/// Decodes a turn frame into its agent, session and turn.
fn turn_frame(frame: &Frame) -> Result<(String, String, Turn)> {
    let corrupt = |_| EngineError::rejected(ErrorKind::CorruptIndex);
    let decoded = decode(&frame.payload).map_err(corrupt)?;
    let turn = Turn {
        lsn: frame.header.lsn,
        seq: decoded.seq,
        commit_time_ns: frame.header.commit_time_ns,
        role: decoded.turn.role,
        content: decoded.turn.content,
        meta: decoded.turn.meta,
    };
    Ok((decoded.agent_id, decoded.session_id, turn))
}

impl Hit {
    /// The record's LSN.
    pub fn lsn(&self) -> u64 {
        match self {
            Self::Turn { turn, .. } => turn.lsn,
            Self::Fact(fact) => fact.lsn,
        }
    }
}

/// Harman's S-stemmer, as Lucene's `EnglishMinimalStemmer` implements it: plurals only.
/// `keys` gives `key` and `policies` gives `policy`. None when the word is unchanged.
fn singular(word: &str) -> Option<String> {
    let (b, n) = (word.as_bytes(), word.len());
    if n < 3 || b[n - 1] != b's' {
        return None;
    }
    match b[n - 2] {
        b'u' | b's' => None,
        b'e' if n > 3 && b[n - 3] == b'i' && !matches!(b[n - 4], b'a' | b'e') => {
            Some(format!("{}y", &word[..n - 3]))
        }
        b'e' if matches!(b[n - 3], b'i' | b'a' | b'o' | b'e') => None,
        _ => Some(word[..n - 1].to_owned()),
    }
}

fn stored((rev, commit_time_ns, fact): ReadFact) -> StoredFact {
    let Fact { key, agent, content, meta, .. } = fact;
    StoredFact { key, lsn: rev.lsn, commit_time_ns, agent, content, meta }
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
    use atticdb_tape::sim::SimClock;
    use atticdb_tape::{CommittedRecord, SimDisk, StateMachine, TapeOptions};
    use std::sync::mpsc;
    use std::time::Duration;

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
                assert_eq!(error.outcome, atticdb_tape::Outcome::NotCommitted, "{case}");
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
