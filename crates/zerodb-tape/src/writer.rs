//! One writer state machine with threaded and deterministic drivers.

use crate::dir::{read_exact, DataDir, Handle};
use crate::frame::{
    self, Digest, Footer, Frame, Header, Kind, FLAG_HUSK, HEADER_LEN, MAX_FRAME_SIZE, VERSION,
};
use crate::io::{lock, Clock, Io};
use crate::recovery::{recover, RecoveryReport};
use crate::segment::{SegmentWriter, DEFAULT_SEGMENT_SIZE};
use crate::{CommittedRecord, Error, NoopStateMachine, Prepare, Result, StateMachine};
use std::collections::{BTreeMap, VecDeque};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

/// A batch's acknowledgement and persistence requirements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    /// Acknowledge only after all earlier segment writes have synced.
    Durable,
    /// Acknowledge Cache-only batches before sync, without assigning a public LSN.
    Fast,
}

/// A tenant record supplied to an append transaction.
#[derive(Clone, Debug)]
pub struct RecordInput {
    /// Record category. Footer records are reserved for the writer.
    pub kind: Kind,
    /// Tenant identifier used for chain isolation.
    pub tenant_id: u32,
    /// Record content, without its generated salt.
    pub content: Vec<u8>,
}

/// Fast acknowledgements expose neither transaction identifiers nor LSNs.
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
    },
    /// An accepted Cache batch whose persistence remains uncertain.
    Fast,
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
}

impl Default for TapeOptions {
    fn default() -> Self {
        Self { segment_size: DEFAULT_SEGMENT_SIZE, server_address: None, replay_from_lsn: 0 }
    }
}

#[derive(Debug)]
struct PreparedRecord {
    input: RecordInput,
    salt: [u8; 16],
    digest: Digest,
}

/// Caller-side salts, content digests, batch digest, and validation.
#[derive(Debug)]
pub struct PreparedBatch {
    records: Vec<PreparedRecord>,
    digest: Digest,
    mode: Durability,
    size: usize,
    changed: bool,
}

impl PreparedBatch {
    /// Inspects records in transaction order, without their generated salts.
    pub fn records(&self) -> impl ExactSizeIterator<Item = &RecordInput> {
        self.records.iter().map(|record| &record.input)
    }

    /// Rewrites records before framing. The writer validates changes and refreshes their digests.
    ///
    /// Rewritten records must still satisfy the batch size and durability constraints.
    /// Invalid rewrites poison the writer because preparation may have reserved application state.
    pub fn records_mut(&mut self) -> impl ExactSizeIterator<Item = &mut RecordInput> {
        self.changed = true;
        self.records.iter_mut().map(|record| &mut record.input)
    }

    fn validate<'a>(
        records: impl ExactSizeIterator<Item = &'a RecordInput>,
        mode: Durability,
    ) -> std::result::Result<usize, WriteError> {
        if records.len() > 1024 {
            return Err(rejected(Error::BatchTooLarge));
        }
        if records.len() == 0 {
            return Err(rejected(Error::InvalidFrame("empty batch".into())));
        }
        let mut size = HEADER_LEN + 44;
        for record in records {
            if record.kind == Kind::Footer
                || (mode == Durability::Fast && record.kind != Kind::Cache)
            {
                return Err(rejected(Error::InvalidFrame("disallowed record kind".into())));
            }
            size = size
                .checked_add(HEADER_LEN + 16)
                .and_then(|size| size.checked_add(record.content.len()))
                .filter(|&size| size <= MAX_FRAME_SIZE)
                .ok_or_else(|| rejected(Error::BatchTooLarge))?;
        }
        Ok(size)
    }

    fn refresh(&mut self) -> Result<()> {
        if self.changed {
            self.size = Self::validate(self.records(), self.mode).map_err(|error| error.kind)?;
            let mut digests = Vec::with_capacity(self.records.len());
            for record in &mut self.records {
                record.digest = frame::content_digest(&record.salt, &record.input.content);
                digests.push(record.digest);
            }
            self.digest = frame::batch_digest(&digests);
            self.changed = false;
        }
        Ok(())
    }

    fn with_entropy<R: std::io::Read>(
        batch: Vec<RecordInput>,
        mode: Durability,
        mut open: impl FnMut() -> Result<R>,
    ) -> std::result::Result<Self, WriteError> {
        let mut source = None;
        Self::with_salts(batch, mode, || {
            let mut reader = match source.take() {
                Some(reader) => reader,
                None => open()?,
            };
            let mut salt = [0; 16];
            reader.read_exact(&mut salt)?;
            source = Some(reader);
            Ok(salt)
        })
    }

    /// Supplies deterministic salts for simulations, without changing the writer.
    pub fn with_salts(
        batch: Vec<RecordInput>,
        mode: Durability,
        mut salt: impl FnMut() -> Result<[u8; 16]>,
    ) -> std::result::Result<Self, WriteError> {
        let size = Self::validate(batch.iter(), mode)?;
        let mut records = Vec::with_capacity(batch.len());
        let mut digests = Vec::with_capacity(batch.len());
        for record in batch {
            let salt = salt().map_err(rejected)?;
            let digest = frame::content_digest(&salt, &record.content);
            digests.push(digest);
            records.push(PreparedRecord { input: record, salt, digest });
        }
        Ok(Self { records, digest: frame::batch_digest(&digests), mode, size, changed: false })
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
    index: Mutex<BTreeMap<u64, (u64, u64)>>,
    durable_lsn: AtomicU64,
    poisoned: AtomicBool,
}

impl ReadState {
    fn scan(&self, from_lsn: u64) -> impl Iterator<Item = Result<CommittedRecord>> + '_ {
        let end = self.durable_lsn.load(Ordering::Acquire);
        let mut next = Some(from_lsn);
        std::iter::from_fn(move || loop {
            let start = next.filter(|&start| start <= end)?;
            let lsn = match lock(&self.index) {
                Ok(index) => index.range(start..=end).next().map(|(&lsn, _)| lsn),
                Err(error) => {
                    next = None;
                    return Some(Err(error));
                }
            }?;
            next = lsn.checked_add(1);
            match self.read(lsn) {
                Ok(Some(frame)) if frame.header.flags & FLAG_HUSK == 0 => {
                    let h = frame.header;
                    return Some(Ok(CommittedRecord {
                        lsn: h.lsn,
                        txn: h.txn,
                        kind: h.kind,
                        tenant_id: h.tenant_id,
                        commit_time_ns: h.commit_time_ns,
                        payload: Arc::from(&frame.payload[16..]),
                    }));
                }
                Ok(Some(_)) => continue,
                Ok(None) => return None,
                Err(error) => {
                    next = None;
                    return Some(Err(error));
                }
            }
        })
    }

    fn read(&self, lsn: u64) -> Result<Option<Frame>> {
        if lsn > self.durable_lsn.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Some((segment, offset)) = lock(&self.index)?.get(&lsn).copied() else {
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
        let (frame, _) =
            frame::decode(&bytes).map_err(|e| crate::dir::corruption(lsn, segment, offset, e))?;
        Ok(Some(frame))
    }
}

/// Enqueue requests, then step once to drain and commit the current group.
pub struct WriterCore<S: StateMachine = NoopStateMachine> {
    segments: SegmentWriter,
    clock: Arc<dyn Clock>,
    reads: Arc<ReadState>,
    queue: VecDeque<WriteRequest>,
    chains: BTreeMap<u32, Digest>,
    pending: BTreeMap<u64, (u64, u64)>,
    unpublished: Vec<CommittedRecord>,
    state_machine: S,
    lsn: u64,
    txn: u64,
    superblock_time: u64,
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
        if opts.segment_size < (HEADER_LEN * 2 + 60) as u64
            || usize::try_from(opts.segment_size).is_err()
        {
            return Err(Error::Configuration("segment cannot hold a transaction".into()));
        }
        let mut dir =
            DataDir::open(io.clone(), path, opts.server_address.as_deref(), opts.segment_size)?;
        let recovered = recover(&mut dir, opts.segment_size)?;
        if recovered.report.durable_lsn > dir.superblock() {
            dir.install_superblock(recovered.report.durable_lsn)?;
        }
        let reads = Arc::new(ReadState {
            io,
            path: dir.path.clone(),
            index: Mutex::new(recovered.index),
            durable_lsn: AtomicU64::new(recovered.report.durable_lsn),
            poisoned: AtomicBool::new(false),
        });
        if let Some(from) = opts.replay_from_lsn.checked_add(1) {
            for record in reads.scan(from) {
                state_machine.replay(&record?)?;
            }
        }
        let segments = SegmentWriter::new(dir, opts.segment_size, recovered.offset)?;
        let core = Self {
            segments,
            superblock_time: clock.now_ns(),
            clock,
            reads,
            queue: VecDeque::new(),
            chains: recovered.chains,
            pending: BTreeMap::new(),
            unpublished: Vec::new(),
            state_machine,
            lsn: recovered.report.durable_lsn,
            txn: recovered.txn,
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
        let time = self.clock.now_ns();
        let count = batch.records.len();
        let mut bytes = Vec::with_capacity(batch.size);
        let mut offsets = Vec::with_capacity(count);
        let mut committed = Vec::with_capacity(count);
        for (i, record) in batch.records.into_iter().enumerate() {
            let input = record.input;
            let prev = if input.kind == Kind::Cache {
                [0; 32]
            } else {
                self.chains.get(&input.tenant_id).copied().unwrap_or([0; 32])
            };
            let header = Header {
                kind: input.kind,
                version: VERSION,
                flags: 0,
                txn,
                lsn: first_lsn + i as u64,
                commit_time_ns: time,
                tenant_id: input.tenant_id,
                tenant_prev_chain: prev,
                content_digest: record.digest,
            };
            if input.kind != Kind::Cache {
                self.chains
                    .insert(input.tenant_id, frame::chain_next(&prev, &header, &record.digest));
            }
            committed.push(CommittedRecord {
                lsn: header.lsn,
                txn,
                kind: input.kind,
                tenant_id: input.tenant_id,
                commit_time_ns: time,
                payload: Arc::from(input.content.as_slice()),
            });
            offsets.push((header.lsn, bytes.len() as u64));
            let mut payload = record.salt.to_vec();
            payload.extend_from_slice(&input.content);
            bytes.extend(frame::encode(&Frame { header, payload })?);
        }
        let footer = Footer { txn, record_count: count as u32, batch_digest: batch.digest };
        bytes.extend(frame::encode(&Frame {
            header: Header {
                kind: Kind::Footer,
                version: VERSION,
                flags: 0,
                txn,
                lsn: last_lsn,
                commit_time_ns: time,
                tenant_id: 0,
                tenant_prev_chain: [0; 32],
                content_digest: [0; 32],
            },
            payload: footer.encode().to_vec(),
        })?);
        let (segment, offset) = self.segments.write_batch(&bytes)?;
        for (lsn, off) in offsets {
            self.pending.insert(lsn, (segment, offset + off));
        }
        self.unpublished.extend(committed);
        self.lsn = last_lsn;
        self.txn = txn;
        Ok(Committed::Durable { txn, first_lsn, last_lsn })
    }

    fn publish(&mut self) -> Result<()> {
        lock(&self.reads.index)?.append(&mut self.pending);
        self.reads.durable_lsn.store(self.lsn, Ordering::Release);
        if !self.unpublished.is_empty() {
            self.state_machine.publish(&self.unpublished);
            self.unpublished.clear();
        }
        Ok(())
    }

    /// Writes all queued transactions, then syncs all touched files before durable replies.
    pub fn step(&mut self) {
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
                    let _ = request.reply.send(Ok(committed));
                    continue;
                }
            }
            let mode = request.batch.mode;
            match self.write(request.batch) {
                Ok(committed) if mode == Durability::Durable => {
                    replies.push((request.reply, committed))
                }
                Ok(_) => {
                    let _ = request.reply.send(Ok(Committed::Fast));
                }
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

    /// Called after replies. Metadata failure poisons future writes, not earlier acknowledgements.
    pub fn update_superblock(&mut self) -> Result<()> {
        let now = self.clock.now_ns();
        if self.is_poisoned() || now.saturating_sub(self.superblock_time) < 1_000_000_000 {
            return Ok(());
        }
        self.checkpoint()
    }

    fn checkpoint(&mut self) -> Result<()> {
        if self.durable_lsn() <= self.segments.dir.superblock() {
            return Ok(());
        }
        self.superblock_time = self.clock.now_ns();
        let result = self.segments.dir.install_superblock(self.durable_lsn());
        if result.is_err() {
            self.reads.poisoned.store(true, Ordering::Release);
        }
        result
    }

    /// Drains requests, syncs pending Fast writes, and checkpoints without the periodic time limit.
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

/// Threaded production driver. Dropping the last handle joins the writer and releases its lock.
pub struct Tape<S: StateMachine = NoopStateMachine> {
    sender: Option<mpsc::Sender<WriteRequest>>,
    thread: Option<JoinHandle<()>>,
    reads: Arc<ReadState>,
    state_machine: PhantomData<fn() -> S>,
}

impl<S: StateMachine> std::fmt::Debug for Tape<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tape").field("durable_lsn", &self.durable_lsn()).finish()
    }
}

impl Tape<NoopStateMachine> {
    /// Locks and recovers a directory, then starts its writer thread.
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
    /// Recovers and replays state before starting the writer thread.
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
        let (mut core, report) = WriterCore::open_with(state_machine, io, clock, path, opts)?;
        let reads = core.reads.clone();
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new().name("zerodb-writer".into()).spawn(move || {
            while let Ok(request) = receiver.recv() {
                core.enqueue(request);
                for request in receiver.try_iter() {
                    core.enqueue(request);
                }
                core.step();
                let _ = core.update_superblock();
            }
            let _ = core.shutdown();
        })?;
        Ok((
            Self { sender: Some(sender), thread: Some(thread), reads, state_machine: PhantomData },
            report,
        ))
    }

    /// Atomically appends a nonempty batch using the requested acknowledgement mode.
    pub fn append(&self, batch: Vec<RecordInput>, mode: Durability) -> WriteResult {
        if self.reads.poisoned.load(Ordering::Acquire) {
            return Err(rejected(Error::Poisoned));
        }
        let batch = PreparedBatch::with_entropy(batch, mode, os_entropy)?;
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

    /// Returns the highest record LSN available to durable readers.
    pub fn durable_lsn(&self) -> u64 {
        self.reads.durable_lsn.load(Ordering::Acquire)
    }

    /// Reads a complete durable record, or returns `None` for an unpublished LSN.
    pub fn read(&self, lsn: u64) -> Result<Option<Frame>> {
        self.reads.read(lsn)
    }

    /// Scans durable application records at or above `from_lsn`, in LSN order.
    ///
    /// The durable boundary is captured when you create the iterator.
    /// It reads one indexed frame at a time across segments, skipping footers and redacted husks.
    /// Payloads exclude generated salts. An I/O or decoding error ends iteration.
    pub fn scan(&self, from_lsn: u64) -> impl Iterator<Item = Result<CommittedRecord>> + '_ {
        self.reads.scan(from_lsn)
    }
}

impl<S: StateMachine> Drop for Tape<S> {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
fn os_entropy() -> Result<std::fs::File> {
    Ok(std::fs::File::open("/dev/urandom")?)
}

#[cfg(not(unix))]
fn os_entropy() -> Result<std::fs::File> {
    Err(Error::Io(std::io::ErrorKind::Unsupported, "OS salt source requires Unix".into()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;

    fn records(count: usize) -> Vec<RecordInput> {
        vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: b"hello".to_vec() }; count]
    }

    #[test]
    fn one_entropy_open_supplies_every_salt_in_a_batch() {
        let opens = Cell::new(0);
        let salts: Vec<_> = (0..32 * 16).map(|i| i as u8).collect();
        let batch = PreparedBatch::with_entropy(records(32), Durability::Durable, || {
            opens.set(opens.get() + 1);
            Ok(Cursor::new(salts.clone()))
        })
        .unwrap();
        assert_eq!(opens.get(), 1);
        for (i, record) in batch.records.iter().enumerate() {
            assert_eq!(&record.salt, &salts[i * 16..(i + 1) * 16]);
        }
    }

    #[test]
    fn invalid_batches_do_not_open_entropy_and_short_reads_are_rejected() {
        let opens = Cell::new(0);
        let error = PreparedBatch::with_entropy(records(1025), Durability::Durable, || {
            opens.set(opens.get() + 1);
            Ok(Cursor::new(vec![0; 16]))
        })
        .unwrap_err();
        assert!(matches!(error.kind, Error::BatchTooLarge));
        assert_eq!(opens.get(), 0);
        let error = PreparedBatch::with_entropy(records(2), Durability::Durable, || {
            opens.set(opens.get() + 1);
            Ok(Cursor::new(vec![0; 16]))
        })
        .unwrap_err();
        assert!(matches!(error.kind, Error::Io(std::io::ErrorKind::UnexpectedEof, _)));
        assert_eq!(error.outcome, Outcome::NotCommitted);
        assert_eq!(opens.get(), 1);
    }
}
