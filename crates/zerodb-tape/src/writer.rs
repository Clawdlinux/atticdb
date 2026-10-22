//! One writer state machine with threaded and deterministic drivers.

use crate::dir::{read_exact, DataDir, Handle, SegmentPrepareError};
use crate::frame::{
    self, Digest, Footer, Frame, Header, Kind, HEADER_LEN, MAX_FRAME_SIZE, VERSION,
};
use crate::io::{lock, Clock, Io};
use crate::lsn_index::{LsnIndex, LsnIndexStats, DEFAULT_BUDGET};
use crate::recovery::{recover_with_budget, RecoveryReport};
use crate::segment::{SegmentWriter, DEFAULT_SEGMENT_SIZE};
use crate::{CommittedRecord, Error, NoopStateMachine, Prepare, Result, StateMachine};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A batch's acknowledgement and persistence requirements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    /// Acknowledge only after all earlier segment writes have synced.
    Durable,
}

/// A tenant record supplied to an append transaction.
#[derive(Clone, Debug)]
pub struct RecordInput {
    /// Record category. Footer records are reserved for the writer.
    pub kind: Kind,
    /// Tenant identifier.
    pub tenant_id: u32,
    /// Application content.
    pub content: Vec<u8>,
}

/// Acknowledgements follow sync and publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Committed {
    /// A synced transaction and its inclusive record range.
    Durable {
        /// Transaction identifier.
        txn: u64,
        /// First record's log sequence number.
        first_lsn: u64,
        /// Last record's log sequence number.
        last_lsn: u64,
        /// Application receipt staged during prepare and returned after publication.
        reply: Vec<u8>,
    },
}

/// What an append error proves about the transaction's persistence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The transaction is durably committed.
    Committed,
    /// The transaction wasn't committed.
    NotCommitted,
    /// Recovery may retain or discard the transaction.
    Unknown,
}

/// An append failure with its transaction outcome.
#[derive(Clone, Debug)]
pub struct WriteError {
    /// The underlying validation, storage, or health failure.
    pub kind: Error,
    /// Whether the failed append could have committed.
    pub outcome: Outcome,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:?})", self.kind, self.outcome)
    }
}

impl std::error::Error for WriteError {}

/// An append acknowledgement or a failure with persistence information.
pub type WriteResult = std::result::Result<Committed, WriteError>;

fn rejected(kind: Error) -> WriteError {
    WriteError { kind, outcome: Outcome::NotCommitted }
}

/// Tape configuration checked against the persisted manifest when opening.
#[derive(Clone, Debug)]
pub struct TapeOptions {
    /// Preallocated segment length. A whole transaction must fit within a segment.
    pub segment_size: u64,
    /// Optional diagnostic address written to the directory's lock file.
    pub server_address: Option<String>,
    /// Replay records strictly above this checkpoint watermark. Zero replays the entire tape.
    pub replay_from_lsn: u64,
    /// Maximum group-formation wait. Zero disables waiting. Cannot exceed 200 microseconds.
    pub coalescing_window: Duration,
    /// Maximum active-write interval between superblock requests. Must be nonzero.
    pub superblock_interval: Duration,
    /// Published tape bytes between superblock requests, including roll padding. Must be nonzero.
    pub superblock_bytes: u64,
    /// Resident LSN-index budget, including page bookkeeping. Minimum 4224 bytes.
    pub lsn_index_bytes: usize,
    /// Bytes kept free after group bytes and any new segments. Defaults to min(1 GiB, 5% of capacity).
    pub free_space_floor: Option<u64>,
    /// Runs the writer inline. Only for deterministic simulations.
    #[cfg(feature = "sim")]
    pub inline_sim: bool,
    /// Optional pause between application publication and tape publication. Simulations only.
    #[cfg(feature = "sim")]
    pub publish_pause: Option<Arc<crate::sim::PublishPause>>,
}

impl Default for TapeOptions {
    fn default() -> Self {
        Self {
            segment_size: DEFAULT_SEGMENT_SIZE,
            server_address: None,
            replay_from_lsn: 0,
            coalescing_window: Duration::from_micros(200),
            superblock_interval: Duration::from_secs(30),
            superblock_bytes: 256 * 1024 * 1024,
            lsn_index_bytes: DEFAULT_BUDGET,
            free_space_floor: None,
            #[cfg(feature = "sim")]
            inline_sim: false,
            #[cfg(feature = "sim")]
            publish_pause: None,
        }
    }
}

/// Caller-side validation and an optional application digest.
#[derive(Debug)]
pub struct PreparedBatch {
    records: Vec<RecordInput>,
    size: usize,
    changed: bool,
    reply: Vec<u8>,
    application_digest: Option<Digest>,
    application_arg: Option<u64>,
}

impl PreparedBatch {
    /// Returns an opaque application digest computed by the caller, if supplied.
    pub fn application_digest(&self) -> Option<Digest> {
        self.application_digest
    }
    /// Returns an opaque caller argument for preparation, if supplied. It is never framed.
    pub fn application_arg(&self) -> Option<u64> {
        self.application_arg
    }
    /// Stages an application receipt. Durable replies release it only after sync and publication.
    pub fn set_reply(&mut self, reply: Vec<u8>) {
        self.reply = reply;
    }
    /// Inspects records in transaction order.
    pub fn records(&self) -> impl ExactSizeIterator<Item = &RecordInput> {
        self.records.iter()
    }

    /// Rewrites records before framing. The writer validates changes.
    ///
    /// Rewrites may shrink or preserve the submitted encoded size, but must not grow it.
    /// Invalid rewrites poison the writer because preparation may have reserved application state.
    pub fn records_mut(&mut self) -> impl ExactSizeIterator<Item = &mut RecordInput> {
        self.changed = true;
        self.records.iter_mut()
    }

    fn validate<'a>(
        records: impl ExactSizeIterator<Item = &'a RecordInput>,
        _mode: Durability,
    ) -> std::result::Result<usize, WriteError> {
        if records.len() > 1024 {
            return Err(rejected(Error::BatchTooLarge));
        }
        if records.len() == 0 {
            return Err(rejected(Error::InvalidFrame("empty batch".into())));
        }
        let mut size = HEADER_LEN + 32;
        for record in records {
            if record.kind == Kind::Footer {
                return Err(rejected(Error::InvalidFrame("disallowed record kind".into())));
            }
            size = size
                .checked_add(HEADER_LEN)
                .and_then(|size| size.checked_add(record.content.len()))
                .filter(|&size| size <= MAX_FRAME_SIZE)
                .ok_or_else(|| rejected(Error::BatchTooLarge))?;
        }
        Ok(size)
    }

    /// Revalidates rewrites against the size budgeted before preparation.
    fn refresh(&mut self) -> Result<()> {
        if self.changed {
            let size =
                Self::validate(self.records(), Durability::Durable).map_err(|error| error.kind)?;
            if size > self.size {
                return Err(Error::InvalidFrame(format!(
                    "prepared batch grew from {} to {size} bytes ({} records)",
                    self.size,
                    self.records.len()
                )));
            }
            self.size = size;
            self.changed = false;
        }
        Ok(())
    }

    /// Validates a transaction before enqueueing.
    pub fn new(batch: Vec<RecordInput>, mode: Durability) -> std::result::Result<Self, WriteError> {
        let size = Self::validate(batch.iter(), mode)?;
        Ok(Self {
            records: batch,
            size,
            changed: false,
            reply: Vec::new(),
            application_digest: None,
            application_arg: None,
        })
    }
}

#[derive(Debug)]
/// A prepared transaction and its acknowledgement channel.
pub struct WriteRequest {
    batch: PreparedBatch,
    reply: mpsc::Sender<WriteResult>,
}

impl WriteRequest {
    /// Creates an enqueueable request and its reply receiver.
    pub fn new(batch: PreparedBatch) -> (Self, mpsc::Receiver<WriteResult>) {
        let (reply, receiver) = mpsc::channel();
        (Self { batch, reply }, receiver)
    }
}

struct ReadState {
    io: Arc<dyn Io>,
    path: PathBuf,
    index: Mutex<LsnIndex>,
    durable_lsn: AtomicU64,
    poisoned: AtomicBool,
    read_only: AtomicBool,
    groups: AtomicU64,
    grouped_requests: AtomicU64,
    superblock_installs: AtomicU64,
}

impl ReadState {
    fn scan(&self, from_lsn: u64) -> impl Iterator<Item = Result<CommittedRecord>> + '_ {
        let end = self.durable_lsn.load(Ordering::Acquire);
        let mut next = Some(from_lsn);
        std::iter::from_fn(move || {
            let start = next.filter(|&start| start <= end)?;
            let lsn = start.max(1);
            if lsn > end {
                return None;
            }
            next = lsn.checked_add(1);
            match self.read(lsn) {
                Ok(Some(frame)) => {
                    let h = frame.header;
                    Some(Ok(CommittedRecord {
                        lsn: h.lsn,
                        txn: h.txn,
                        kind: h.kind,
                        tenant_id: h.tenant_id,
                        commit_time_ns: h.commit_time_ns,
                        payload: Arc::from(frame.payload),
                    }))
                }
                Ok(None) => None,
                Err(error) => {
                    next = None;
                    Some(Err(error))
                }
            }
        })
    }

    fn read(&self, lsn: u64) -> Result<Option<Frame>> {
        if lsn > self.durable_lsn.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Some((segment, offset)) = lock(&self.index)?.get(lsn)? else {
            return Ok(None);
        };
        let file =
            Handle::open(self.io.clone(), &self.path.join(format!("seg-{segment:016}.zdb")))?;
        let mut header = [0; HEADER_LEN];
        read_exact(self.io.as_ref(), file.id, offset, &mut header)?;
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if !(HEADER_LEN..=MAX_FRAME_SIZE).contains(&len) {
            return Err(crate::dir::corruption(lsn, segment, offset, "invalid read length"));
        }
        let mut bytes = vec![0; len];
        bytes[..HEADER_LEN].copy_from_slice(&header);
        read_exact(
            self.io.as_ref(),
            file.id,
            offset + HEADER_LEN as u64,
            &mut bytes[HEADER_LEN..],
        )?;
        let (frame, _) = frame::decode(&bytes, segment, offset)
            .map_err(|e| crate::dir::corruption(lsn, segment, offset, e))?;
        if frame.header.lsn != lsn || frame.header.kind == Kind::Footer {
            return Err(crate::dir::corruption(
                lsn,
                segment,
                offset,
                "LSN index points to another record",
            ));
        }
        Ok(Some(frame))
    }
}

/// Enqueue requests, then step once to drain and commit the current group.
pub struct WriterCore<S: StateMachine = NoopStateMachine> {
    segments: SegmentWriter,
    clock: Arc<dyn Clock>,
    reads: Arc<ReadState>,
    queue: VecDeque<WriteRequest>,
    pending: Vec<(u64, u64, u64)>,
    unpublished: Vec<CommittedRecord>,
    unpublished_bytes: u64,
    state_machine: S,
    lsn: u64,
    txn: u64,
    commit_time_ns: u64,
    superblock_time: u64,
    superblock_interval_ns: u64,
    superblock_bytes: u64,
    superblock_position: (u64, u64),
    synced_position: (u64, u64),
    free_space_floor: Option<u64>,
    #[cfg(feature = "sim")]
    publish_pause: Option<Arc<crate::sim::PublishPause>>,
}

impl<S: StateMachine> std::fmt::Debug for WriterCore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterCore").field("lsn", &self.lsn).field("txn", &self.txn).finish()
    }
}

#[cfg(any(test, feature = "sim"))]
impl WriterCore<NoopStateMachine> {
    /// Locks the directory, recovers complete transactions, and prepares the active segment.
    pub fn open(
        io: Arc<dyn Io>,
        clock: Arc<dyn Clock>,
        path: impl AsRef<Path>,
        opts: TapeOptions,
    ) -> Result<(Self, RecoveryReport)> {
        Self::open_with(NoopStateMachine, io, clock, path, opts)
    }
}

impl<S: StateMachine> WriterCore<S> {
    /// Recovers and replays durable records before accepting requests.
    pub fn open_with(
        mut state_machine: S,
        io: Arc<dyn Io>,
        clock: Arc<dyn Clock>,
        path: impl AsRef<Path>,
        opts: TapeOptions,
    ) -> Result<(Self, RecoveryReport)> {
        if opts.coalescing_window > Duration::from_micros(200) {
            return Err(Error::Configuration("coalescing window exceeds 200 microseconds".into()));
        }
        if opts.superblock_interval.is_zero() || opts.superblock_bytes == 0 {
            return Err(Error::Configuration(
                "superblock interval and byte limit must be positive".into(),
            ));
        }
        if opts.segment_size < (HEADER_LEN * 2 + 32) as u64
            || usize::try_from(opts.segment_size).is_err()
        {
            return Err(Error::Configuration("segment cannot hold a transaction".into()));
        }
        let mut dir =
            DataDir::open(io.clone(), path, opts.server_address.as_deref(), opts.segment_size)?;
        let free_space_floor = opts.free_space_floor.or_else(|| {
            io.total_bytes(&dir.path).ok().map(|capacity| (capacity / 20).min(1 << 30))
        });
        let mut recovered = recover_with_budget(&mut dir, opts.segment_size, opts.lsn_index_bytes)?;
        if recovered.report.durable_lsn > dir.superblock() {
            dir.install_superblock(recovered.report.durable_lsn)?;
        }
        let reads = Arc::new(ReadState {
            io,
            path: dir.path.clone(),
            index: Mutex::new(recovered.index.take().ok_or(Error::Poisoned)?),
            durable_lsn: AtomicU64::new(recovered.report.durable_lsn),
            poisoned: AtomicBool::new(false),
            read_only: AtomicBool::new(false),
            groups: AtomicU64::new(0),
            grouped_requests: AtomicU64::new(0),
            superblock_installs: AtomicU64::new(0),
        });
        if let Some(from) = opts.replay_from_lsn.checked_add(1) {
            for record in reads.scan(from) {
                state_machine.replay(&record?)?;
            }
        }
        let segments = SegmentWriter::new(dir, opts.segment_size, recovered.offset)?;
        // Finish recovery before optional reservations consume any remaining headroom.
        let highest_lsn = recovered.report.durable_lsn.max(1);
        let needed = lock(&reads.index)?.reservation_bytes(highest_lsn)?;
        let read_only = match free_space_floor {
            Some(floor) => match reads.io.available_bytes(&reads.path) {
                Ok(free) if free.checked_sub(needed).is_some_and(|left| left >= floor) => {
                    match lock(&reads.index)?.reserve(highest_lsn) {
                        Ok(()) => false,
                        Err(Error::DiskFull | Error::Io(std::io::ErrorKind::StorageFull, _)) => {
                            true
                        }
                        Err(error) => return Err(error),
                    }
                }
                _ => true,
            },
            None => true,
        };
        reads.read_only.store(read_only, Ordering::Release);
        let position = segments.position();
        let core = Self {
            segments,
            superblock_time: clock.now_ns(),
            superblock_interval_ns: opts.superblock_interval.as_nanos().min(u128::from(u64::MAX))
                as u64,
            superblock_bytes: opts.superblock_bytes,
            superblock_position: position,
            synced_position: position,
            free_space_floor,
            #[cfg(feature = "sim")]
            publish_pause: opts.publish_pause,
            clock,
            reads,
            queue: VecDeque::new(),
            pending: Vec::new(),
            unpublished: Vec::new(),
            unpublished_bytes: 0,
            state_machine,
            lsn: recovered.report.durable_lsn,
            txn: recovered.txn,
            commit_time_ns: recovered.commit_time_ns,
        };
        Ok((core, recovered.report))
    }

    /// Adds a request to the next group without performing I/O.
    pub fn enqueue(&mut self, request: WriteRequest) {
        self.queue.push_back(request);
    }

    /// Returns the last record visible to durable readers.
    pub fn durable_lsn(&self) -> u64 {
        self.reads.durable_lsn.load(Ordering::Acquire)
    }

    /// Reports whether a storage failure requires reopening.
    pub fn is_poisoned(&self) -> bool {
        self.reads.poisoned.load(Ordering::Acquire)
    }

    /// True after a failed space check or full-disk preallocation. A passing space check clears it.
    pub fn read_only(&self) -> bool {
        self.reads.read_only.load(Ordering::Acquire)
    }

    /// Returns charged index memory and backing-page reads.
    #[cfg(any(test, feature = "sim"))]
    pub fn index_stats(&self) -> Result<LsnIndexStats> {
        Ok(lock(&self.reads.index)?.stats())
    }

    /// Reads a published record through the deterministic driver.
    #[cfg(any(test, feature = "sim"))]
    pub fn read(&self, lsn: u64) -> Result<Option<Frame>> {
        self.reads.read(lsn)
    }

    /// Scans a snapshot of the durable boundary through the deterministic driver.
    #[cfg(any(test, feature = "sim"))]
    pub fn scan(&self, from_lsn: u64) -> impl Iterator<Item = Result<CommittedRecord>> + '_ {
        self.reads.scan(from_lsn)
    }

    fn write(&mut self, mut batch: PreparedBatch) -> Result<Committed> {
        batch.refresh()?;
        let txn =
            self.txn.checked_add(1).ok_or_else(|| Error::InvalidFrame("txn exhausted".into()))?;
        let first_lsn =
            self.lsn.checked_add(1).ok_or_else(|| Error::InvalidFrame("LSN exhausted".into()))?;
        let last_lsn = self
            .lsn
            .checked_add(batch.records.len() as u64)
            .ok_or_else(|| Error::InvalidFrame("LSN exhausted".into()))?;
        let time = self
            .commit_time_ns
            .checked_add(1)
            .ok_or_else(|| Error::InvalidFrame("commit time exhausted".into()))?
            .max(self.clock.now_ns());
        let count = batch.records.len();
        let (segment, offset) = self.segments.reserve_batch(batch.size)?;
        let mut bytes = Vec::with_capacity(batch.size);
        let mut offsets = Vec::with_capacity(count);
        let mut committed = Vec::with_capacity(count);
        let mut records_crc = crate::crc32c::Crc32c::new();
        for (i, input) in batch.records.into_iter().enumerate() {
            let header = Header {
                kind: input.kind,
                version: VERSION,
                flags: 0,
                txn,
                lsn: first_lsn + i as u64,
                commit_time_ns: time,
                tenant_id: input.tenant_id,
            };
            committed.push(CommittedRecord {
                lsn: header.lsn,
                txn,
                kind: input.kind,
                tenant_id: input.tenant_id,
                commit_time_ns: time,
                payload: Arc::from(input.content.as_slice()),
            });
            offsets.push((header.lsn, bytes.len() as u64));
            let encoded = frame::encode(
                &Frame { header, payload: input.content },
                segment,
                offset + bytes.len() as u64,
            )?;
            records_crc.update(&encoded[..4]);
            bytes.extend(encoded);
        }
        let footer = Footer {
            txn,
            first_lsn,
            synced_lsn: self.durable_lsn(),
            record_count: count as u32,
            records_crc: records_crc.finish(),
        };
        bytes.extend(frame::encode(
            &Frame {
                header: Header {
                    kind: Kind::Footer,
                    version: VERSION,
                    flags: 0,
                    txn,
                    lsn: last_lsn,
                    commit_time_ns: time,
                    tenant_id: 0,
                },
                payload: footer.encode().to_vec(),
            },
            segment,
            offset + bytes.len() as u64,
        )?);
        self.segments.write_batch(&bytes)?;
        for (lsn, off) in offsets {
            self.pending.push((lsn, segment, offset + off));
        }
        self.unpublished.extend(committed);
        self.unpublished_bytes = self.unpublished_bytes.saturating_add(bytes.len() as u64);
        self.lsn = last_lsn;
        self.txn = txn;
        self.commit_time_ns = time;
        Ok(Committed::Durable { txn, first_lsn, last_lsn, reply: batch.reply })
    }

    fn publish(&mut self) -> Result<()> {
        let mut index = lock(&self.reads.index)?;
        for (lsn, segment, offset) in self.pending.drain(..) {
            index.insert(lsn, segment, offset)?;
        }
        drop(index);
        self.synced_position = self.segments.position();
        if !self.unpublished.is_empty() {
            self.state_machine.publish(&self.unpublished);
            #[cfg(feature = "sim")]
            if let Some(pause) = &self.publish_pause {
                pause.pause();
            }
            self.unpublished.clear();
        }
        self.reads.durable_lsn.store(self.lsn, Ordering::Release);
        self.unpublished_bytes = 0;
        Ok(())
    }

    /// Writes queued transactions and syncs before durable replies.
    pub fn step(&mut self) {
        if !self.queue.is_empty() {
            self.reads.groups.fetch_add(1, Ordering::Relaxed);
            self.reads.grouped_requests.fetch_add(self.queue.len() as u64, Ordering::Relaxed);
        }
        if !self.is_poisoned() {
            let capacity = self.segments.capacity();
            self.queue.retain(|request| {
                if request.batch.size as u64 > capacity {
                    let _ = request.reply.send(Err(rejected(Error::BatchTooLarge)));
                    false
                } else {
                    true
                }
            });
            if !self.queue.is_empty() {
                let floor = match self.free_space_floor {
                    Some(floor) => floor,
                    None => match self.reads.io.total_bytes(&self.reads.path) {
                        Ok(capacity) => {
                            let floor = (capacity / 20).min(1 << 30);
                            self.free_space_floor = Some(floor);
                            floor
                        }
                        Err(error) => {
                            self.reads.read_only.store(true, Ordering::Release);
                            self.reject_group(error);
                            return;
                        }
                    },
                };
                let Some(highest_lsn) = self.queue.iter().try_fold(self.durable_lsn(), |lsn, r| {
                    lsn.checked_add(r.batch.records.len() as u64)
                }) else {
                    self.reject_group(Error::Rejected("lsn_index"));
                    return;
                };
                let index_space =
                    lock(&self.reads.index).and_then(|index| index.reservation_bytes(highest_lsn));
                let index_space = match index_space {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        self.reject_storage_group(error);
                        return;
                    }
                };
                let needed = self
                    .segments
                    .group_space(self.queue.iter().map(|r| r.batch.size as u64))
                    .saturating_add(index_space);
                let available = self.reads.io.available_bytes(&self.reads.path);
                match available {
                    Ok(free) if free.checked_sub(needed).is_none_or(|left| left < floor) => {
                        self.reads.read_only.store(true, Ordering::Release);
                        self.reject_group(Error::DiskFull);
                        return;
                    }
                    Ok(_) => self.reads.read_only.store(false, Ordering::Release),
                    Err(error) => {
                        self.reads.read_only.store(true, Ordering::Release);
                        self.reject_group(error);
                        return;
                    }
                }
                let reserved =
                    lock(&self.reads.index).and_then(|mut index| index.reserve(highest_lsn));
                if let Err(error) = reserved {
                    if matches!(
                        error,
                        Error::DiskFull | Error::Io(std::io::ErrorKind::StorageFull, _)
                    ) {
                        self.reads.read_only.store(true, Ordering::Release);
                        self.reject_group(Error::DiskFull);
                    } else {
                        self.reject_storage_group(error);
                    }
                    return;
                }
                if let Err(error) =
                    self.segments.prepare_group(self.queue.iter().map(|r| r.batch.size as u64))
                {
                    match error {
                        SegmentPrepareError::NoSpace(_) => {
                            self.reads.read_only.store(true, Ordering::Release);
                            self.reject_group(Error::DiskFull);
                        }
                        SegmentPrepareError::Storage(error) => self.reject_storage_group(error),
                    }
                    return;
                }
                // Activate the first placement before application preparation.
                if let Err(error) = self.segments.reserve_batch(self.queue[0].batch.size) {
                    self.reject_storage_group(error);
                    return;
                }
            }
        }
        let mut replies = Vec::new();
        while let Some(mut request) = self.queue.pop_front() {
            if self.is_poisoned() {
                let _ = request.reply.send(Err(rejected(Error::Poisoned)));
                continue;
            }
            if request.batch.size as u64 > self.segments.capacity() {
                let _ = request.reply.send(Err(rejected(Error::BatchTooLarge)));
                continue;
            }
            match self.state_machine.prepare(&mut request.batch) {
                Prepare::Write => {}
                Prepare::Reject(error) => {
                    let _ = request.reply.send(Err(error));
                    continue;
                }
                Prepare::Reply(committed) => {
                    replies.push((request.reply, committed));
                    continue;
                }
            }
            match self.write(request.batch) {
                Ok(committed) => replies.push((request.reply, committed)),
                Err(error) => {
                    let error = match error {
                        Error::Io(std::io::ErrorKind::StorageFull, _) => Error::DiskFull,
                        error => error,
                    };
                    self.reads.poisoned.store(true, Ordering::Release);
                    let result = if matches!(error, Error::SyncFailed(_)) {
                        Err(error.clone())
                    } else {
                        self.segments
                            .discard_partial()
                            .and_then(|()| self.segments.sync())
                            .and_then(|()| self.publish())
                    };
                    let outcome =
                        if result.is_err() { Outcome::Unknown } else { Outcome::NotCommitted };
                    let _ = request.reply.send(Err(WriteError { kind: error.clone(), outcome }));
                    for (reply, committed) in replies.drain(..) {
                        let _ = reply.send(if result.is_ok() {
                            Ok(committed)
                        } else {
                            Err(WriteError { kind: error.clone(), outcome: Outcome::Unknown })
                        });
                    }
                }
            }
        }
        self.segments.finish_group();
        if self.is_poisoned() {
            for (reply, _) in replies {
                let _ = reply
                    .send(Err(WriteError { kind: Error::Poisoned, outcome: Outcome::Unknown }));
            }
            return;
        }
        if replies.is_empty() {
            return;
        }
        let result = self.segments.sync().and_then(|()| self.publish());
        if result.is_err() {
            self.reads.poisoned.store(true, Ordering::Release);
        }
        for (reply, committed) in replies {
            let _ = reply.send(match &result {
                Ok(()) => Ok(committed),
                Err(error) => Err(WriteError { kind: error.clone(), outcome: Outcome::Unknown }),
            });
        }
    }

    fn reject_storage_group(&mut self, error: Error) {
        self.reads.poisoned.store(true, Ordering::Release);
        let error = match error {
            Error::Io(std::io::ErrorKind::StorageFull, _) => Error::DiskFull,
            error => error,
        };
        self.reject_group(error);
    }

    fn reject_group(&mut self, error: Error) {
        for request in self.queue.drain(..) {
            let _ = request.reply.send(Err(rejected(error.clone())));
        }
        self.segments.finish_group();
    }

    /// Called after replies. Metadata failure poisons future writes, not earlier acknowledgements.
    #[cfg(any(test, feature = "sim"))]
    pub fn update_superblock(&mut self) -> Result<()> {
        if !self.superblock_due() {
            return Ok(());
        }
        self.checkpoint()
    }
    /// Runs the maintenance worker's steps inline. Spare failures retry at the next call.
    #[cfg(any(test, feature = "sim"))]
    pub fn maintain(&mut self) -> Result<()> {
        self.update_superblock()?;
        if !self.is_poisoned() {
            let _ = self.make_spare();
        }
        Ok(())
    }
    fn make_spare(&self) -> Result<()> {
        // Without a floor the writer is read-only, so skip the fill.
        let floor = self.free_space_floor.unwrap_or(u64::MAX);
        crate::dir::make_spare(
            self.reads.io.clone(),
            &self.reads.path,
            self.segments.capacity(),
            floor,
        )
    }
    fn superblock_due(&self) -> bool {
        !self.is_poisoned()
            && !self.read_only()
            && (self.synced_position.0 != self.superblock_position.0
                || self.synced_position.1.saturating_sub(self.superblock_position.1)
                    >= self.superblock_bytes
                || self.clock.now_ns().saturating_sub(self.superblock_time)
                    >= self.superblock_interval_ns)
    }
    fn note_superblock_request(&mut self) {
        self.superblock_time = self.clock.now_ns();
        self.superblock_position = self.synced_position;
    }

    fn checkpoint(&mut self) -> Result<()> {
        if self.durable_lsn() <= self.segments.dir.superblock() {
            return Ok(());
        }
        self.note_superblock_request();
        let result = self.segments.dir.install_superblock(self.durable_lsn());
        if result.is_err() {
            self.reads.poisoned.store(true, Ordering::Release);
        } else {
            self.reads.superblock_installs.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// Drains requests and checkpoints without the periodic time limit.
    pub fn shutdown(&mut self) -> Result<()> {
        self.step();
        if self.is_poisoned() {
            return Err(Error::Poisoned);
        }
        let result =
            self.segments.sync().and_then(|()| self.publish()).and_then(|()| self.checkpoint());
        if result.is_err() {
            self.reads.poisoned.store(true, Ordering::Release);
        }
        result
    }
}

/// Bounded adaptive group formation, shared with the bare benchmark driver.
#[derive(Debug)]
pub struct Coalescer {
    window: Duration,
    recent: VecDeque<usize>,
}
impl Coalescer {
    /// Creates a driver with at most 200 microseconds of waiting per group.
    pub fn new(window: Duration) -> Self {
        Self { window: window.min(Duration::from_micros(200)), recent: VecDeque::new() }
    }
    /// Drains ready requests, then waits for the largest of the previous 64 groups.
    pub fn collect<T>(&mut self, receiver: &mpsc::Receiver<T>, first: T) -> Vec<T> {
        let mut group = vec![first];
        group.extend(receiver.try_iter());
        self.collect_with(
            group,
            |wait| receiver.recv_timeout(wait).ok(),
            || receiver.try_iter().collect(),
        )
    }
    fn collect_with<T>(
        &mut self,
        mut group: Vec<T>,
        mut receive: impl FnMut(Duration) -> Option<T>,
        mut ready: impl FnMut() -> Vec<T>,
    ) -> Vec<T> {
        let target = self.recent.iter().copied().max().unwrap_or(1);
        let start = Instant::now();
        while group.len() < target && !self.window.is_zero() {
            let remaining = self.window.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break;
            }
            match receive(remaining) {
                Some(request) => group.push(request),
                None => break,
            }
        }
        // Scheduling can outlast the wait budget. Include arrivals without another blocking wait.
        group.extend(ready());
        self.recent.push_back(group.len());
        if self.recent.len() > 64 {
            self.recent.pop_front();
        }
        group
    }
}

/// Installs due superblocks, then keeps one spare segment ready.
struct MaintenanceWorker {
    sender: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}
impl MaintenanceWorker {
    fn start<S: StateMachine>(core: &WriterCore<S>) -> Result<Self> {
        let reads = core.reads.clone();
        let io = reads.io.clone();
        let path = reads.path.clone();
        let size = core.segments.capacity();
        let floor = core.free_space_floor.unwrap_or(u64::MAX);
        let mut installed = core.segments.dir.superblock();
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread =
            std::thread::Builder::new().name("zerodb-maintenance".into()).spawn(move || {
                while receiver.recv().is_ok() {
                    if reads.poisoned.load(Ordering::Acquire) {
                        continue;
                    }
                    let boundary = reads.durable_lsn.load(Ordering::Acquire);
                    if boundary > installed {
                        if let Err(error) =
                            crate::dir::write_superblock(io.clone(), &path, boundary)
                        {
                            reads.poisoned.store(true, Ordering::Release);
                            eprintln!("zerodb: superblock maintenance failed: {error}");
                            continue;
                        }
                        installed = boundary;
                        reads.superblock_installs.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Err(error) = crate::dir::make_spare(io.clone(), &path, size, floor) {
                        eprintln!("zerodb: spare segment fill failed: {error}");
                    }
                }
            })?;
        Ok(Self { sender: Some(sender), thread: Some(thread) })
    }
    fn wake(&self) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(());
        }
    }
}
impl Drop for MaintenanceWorker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Threaded production driver. Dropping the last handle joins the writer and releases its lock.
pub struct Tape<S: StateMachine = NoopStateMachine> {
    sender: Option<mpsc::Sender<WriteRequest>>,
    thread: Option<JoinHandle<()>>,
    reads: Arc<ReadState>,
    state_machine: PhantomData<fn() -> S>,
    #[cfg(feature = "sim")]
    inline: Option<Mutex<WriterCore<S>>>,
}

impl<S: StateMachine> std::fmt::Debug for Tape<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tape").field("durable_lsn", &self.durable_lsn()).finish()
    }
}

impl Tape<NoopStateMachine> {
    /// Explicitly drops complete transactions after `lsn`. Dropped LSNs may be reused.
    ///
    /// The directory must be closed. Mid-transaction cuts are rejected with both valid boundaries.
    pub fn recover_truncate_at(path: impl AsRef<Path>, lsn: u64) -> Result<RecoveryReport> {
        Self::recover_truncate_at_with(Arc::new(crate::StdIo::new()), path, lsn)
    }

    /// Runs operator truncation through a replaceable I/O backend.
    pub fn recover_truncate_at_with(
        io: Arc<dyn Io>,
        path: impl AsRef<Path>,
        lsn: u64,
    ) -> Result<RecoveryReport> {
        let mut dir = DataDir::open_existing(io, path.as_ref())?;
        let size = dir.segment_size();
        crate::recovery::truncate_at(&mut dir, size, lsn)
    }

    /// Locks and recovers a directory, then starts its writer and metadata workers.
    pub fn open(
        io: Arc<dyn Io>,
        clock: Arc<dyn Clock>,
        path: impl AsRef<Path>,
        opts: TapeOptions,
    ) -> Result<(Self, RecoveryReport)> {
        Self::open_with(NoopStateMachine, io, clock, path, opts)
    }
}

impl<S: StateMachine> Tape<S> {
    /// Recovers and replays state before starting background workers.
    ///
    /// `opts.replay_from_lsn` excludes records already represented by your loaded checkpoint.
    pub fn open_with(
        state_machine: S,
        io: Arc<dyn Io>,
        clock: Arc<dyn Clock>,
        path: impl AsRef<Path>,
        opts: TapeOptions,
    ) -> Result<(Self, RecoveryReport)>
    where
        S: 'static,
    {
        #[cfg(feature = "sim")]
        let inline = opts.inline_sim;
        let mut coalescer = Coalescer::new(opts.coalescing_window);
        let (mut core, report) = WriterCore::open_with(state_machine, io, clock, path, opts)?;
        let reads = core.reads.clone();
        // Fill the first spare before any write, so no open-time fill races the writer.
        if let Err(error) = core.make_spare() {
            eprintln!("zerodb: spare segment fill failed: {error}");
        }
        #[cfg(feature = "sim")]
        if inline {
            return Ok((
                Self {
                    sender: None,
                    thread: None,
                    reads,
                    state_machine: PhantomData,
                    inline: Some(Mutex::new(core)),
                },
                report,
            ));
        }
        let (sender, receiver) = mpsc::channel();
        let maintenance = MaintenanceWorker::start(&core)?;
        let thread = std::thread::Builder::new().name("zerodb-writer".into()).spawn(move || {
            while let Ok(request) = receiver.recv() {
                for request in coalescer.collect(&receiver, request) {
                    core.enqueue(request);
                }
                core.step();
                // A roll moves the synced segment, so it is always due and wakes the spare fill.
                if core.superblock_due() {
                    core.note_superblock_request();
                    maintenance.wake();
                }
            }
            // Join periodic installs before the final synchronous install. Never regress the boundary.
            drop(maintenance);
            let _ = core.shutdown();
        })?;
        Ok((
            Self {
                sender: Some(sender),
                thread: Some(thread),
                reads,
                state_machine: PhantomData,
                #[cfg(feature = "sim")]
                inline: None,
            },
            report,
        ))
    }

    /// Atomically appends a nonempty batch using the requested acknowledgement mode.
    pub fn append(&self, batch: Vec<RecordInput>, mode: Durability) -> WriteResult {
        self.append_with_digest(batch, mode, None, None)
    }

    /// Appends with a caller-computed application digest and an unframed preparation argument.
    /// Frame CRCs remain independent.
    pub fn append_with_digest(
        &self,
        batch: Vec<RecordInput>,
        mode: Durability,
        digest: Option<Digest>,
        arg: Option<u64>,
    ) -> WriteResult {
        if self.reads.poisoned.load(Ordering::Acquire) {
            return Err(rejected(Error::Poisoned));
        }
        #[cfg(feature = "sim")]
        if let Some(core) = &self.inline {
            let mut core = core.lock().map_err(|_| rejected(Error::Poisoned))?;
            let mut batch = PreparedBatch::new(batch, mode)?;
            batch.application_digest = digest;
            batch.application_arg = arg;
            let (request, receiver) = WriteRequest::new(batch);
            core.enqueue(request);
            core.step();
            let _ = core.maintain();
            return receiver
                .recv()
                .map_err(|_| WriteError { kind: Error::Poisoned, outcome: Outcome::Unknown })?;
        }
        let mut batch = PreparedBatch::new(batch, mode)?;
        batch.application_digest = digest;
        batch.application_arg = arg;
        let (request, receiver) = WriteRequest::new(batch);
        self.sender
            .as_ref()
            .ok_or_else(|| rejected(Error::Poisoned))?
            .send(request)
            .map_err(|_| rejected(Error::Poisoned))?;
        receiver
            .recv()
            .map_err(|_| WriteError { kind: Error::Poisoned, outcome: Outcome::Unknown })?
    }

    /// Queues one deterministic request without stepping. Requires inline simulation mode.
    #[cfg(feature = "sim")]
    pub fn enqueue_sim(
        &self,
        records: Vec<RecordInput>,
        digest: Option<Digest>,
        arg: Option<u64>,
    ) -> Result<mpsc::Receiver<WriteResult>> {
        let mut batch = PreparedBatch::new(records, Durability::Durable).map_err(|e| e.kind)?;
        batch.application_digest = digest;
        batch.application_arg = arg;
        let (request, receiver) = WriteRequest::new(batch);
        let core = self
            .inline
            .as_ref()
            .ok_or_else(|| Error::Configuration("inline simulation required".into()))?;
        lock(core)?.enqueue(request);
        Ok(receiver)
    }

    /// Commits the queued deterministic group through the production writer state machine.
    #[cfg(feature = "sim")]
    pub fn step_sim(&self) -> Result<()> {
        let core = self
            .inline
            .as_ref()
            .ok_or_else(|| Error::Configuration("inline simulation required".into()))?;
        let mut core = lock(core)?;
        core.step();
        let _ = core.maintain();
        Ok(())
    }

    /// Returns writer group and request counts, including cached replies and rejections.
    pub fn group_stats(&self) -> (u64, u64) {
        (
            self.reads.groups.load(Ordering::Relaxed),
            self.reads.grouped_requests.load(Ordering::Relaxed),
        )
    }

    /// Successful runtime superblock installs, excluding initialization and recovery before open returns.
    pub fn superblock_installs(&self) -> u64 {
        self.reads.superblock_installs.load(Ordering::Relaxed)
    }

    /// Returns charged LSN-index memory and backing-page reads.
    pub fn index_stats(&self) -> Result<LsnIndexStats> {
        Ok(lock(&self.reads.index)?.stats())
    }

    /// Returns the highest record LSN available to durable readers.
    pub fn durable_lsn(&self) -> u64 {
        self.reads.durable_lsn.load(Ordering::Acquire)
    }

    /// True after a failed space check or full-disk preallocation. Reads remain available.
    pub fn read_only(&self) -> bool {
        self.reads.read_only.load(Ordering::Acquire)
    }

    /// True after a storage failure. Reopen before accepting more writes.
    pub fn is_poisoned(&self) -> bool {
        self.reads.poisoned.load(Ordering::Acquire)
    }

    /// Reads a complete durable record, or returns `None` for an unpublished LSN.
    pub fn read(&self, lsn: u64) -> Result<Option<Frame>> {
        self.reads.read(lsn)
    }

    /// Scans durable application records at or above `from_lsn`, in LSN order.
    ///
    /// The durable boundary is captured when you create the iterator.
    /// It reads one indexed record at a time across segments, skipping footers.
    /// An I/O or decoding error ends iteration.
    pub fn scan(&self, from_lsn: u64) -> impl Iterator<Item = Result<CommittedRecord>> + '_ {
        self.reads.scan(from_lsn)
    }
}

impl<S: StateMachine> Drop for Tape<S> {
    fn drop(&mut self) {
        #[cfg(feature = "sim")]
        if let Some(core) = &mut self.inline {
            if let Ok(core) = core.get_mut() {
                let _ = core.shutdown();
            }
        }
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn records(count: usize) -> Vec<RecordInput> {
        vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: b"hello".to_vec() }; count]
    }

    #[test]
    fn asynchronous_poison_mid_step_returns_unknown() {
        struct PoisonDuringPrepare(Option<Box<dyn FnMut() + Send>>);
        impl StateMachine for PoisonDuringPrepare {
            fn prepare(&mut self, _: &mut PreparedBatch) -> Prepare {
                if let Some(callback) = &mut self.0 {
                    callback();
                }
                Prepare::Write
            }
            fn publish(&mut self, _: &[CommittedRecord]) {
                panic!("poisoned data must not publish");
            }
            fn replay(&mut self, _: &CommittedRecord) -> Result<()> {
                Ok(())
            }
        }
        let disk = Arc::new(crate::SimDisk::new(32));
        let opts = TapeOptions { segment_size: 4096, ..TapeOptions::default() };
        let (mut core, _) = WriterCore::open_with(
            PoisonDuringPrepare(None),
            disk,
            Arc::new(crate::sim::SimClock::default()),
            "db",
            opts,
        )
        .unwrap();
        let reads = core.reads.clone();
        core.state_machine.0 =
            Some(Box::new(move || reads.poisoned.store(true, Ordering::Release)));
        let batch = || PreparedBatch::new(records(1), Durability::Durable).unwrap();
        let (first, reply) = WriteRequest::new(batch());
        let (second, later) = WriteRequest::new(batch());
        core.enqueue(first);
        core.enqueue(second);
        core.step();
        let error = reply.recv().unwrap().unwrap_err();
        assert_eq!(error.outcome, Outcome::Unknown);
        assert!(matches!(error.kind, Error::Poisoned));
        assert_eq!(later.recv().unwrap().unwrap_err().outcome, Outcome::NotCommitted);
        assert_eq!(core.durable_lsn(), 0);
        assert!(core.read(1).unwrap().is_none());
    }

    #[test]
    fn coalescing_targets_recent_groups_and_bounds_waiting() {
        let mut groups = Coalescer::new(std::time::Duration::from_micros(200));
        groups.recent.push_back(3);
        let mut next = [2, 3].into_iter();
        assert_eq!(
            groups.collect_with(
                vec![1],
                |wait| {
                    assert!(wait <= std::time::Duration::from_micros(200));
                    next.next()
                },
                Vec::new
            ),
            vec![1, 2, 3]
        );
        groups.window = std::time::Duration::ZERO;
        assert_eq!(
            groups.collect_with(vec![1], |_| panic!("zero disables waiting"), Vec::new),
            vec![1]
        );
        groups.window = std::time::Duration::from_micros(200);
        let mut calls = 0;
        assert_eq!(
            groups.collect_with(
                vec![1],
                |_| {
                    calls += 1;
                    None
                },
                Vec::new
            ),
            vec![1]
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn coalescing_drains_ready_requests_when_waiting_expires() {
        let mut groups = Coalescer::new(Duration::from_micros(200));
        groups.recent.push_back(3);
        let group = groups.collect_with(vec![1], |_| None, || vec![2, 3]);
        assert_eq!(group, vec![1, 2, 3]);
        assert_eq!(groups.recent.back(), Some(&3));
    }
}
