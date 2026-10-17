//! One writer state machine with threaded and deterministic drivers.

use crate::dir::{read_exact, DataDir, Handle};
use crate::frame::{
    self, Digest, Footer, Frame, Header, Kind, HEADER_LEN, MAX_FRAME_SIZE, VERSION,
};
use crate::io::{lock, Clock, Io};
use crate::recovery::{recover, RecoveryReport};
use crate::segment::{SegmentWriter, DEFAULT_SEGMENT_SIZE};
use crate::{Error, Result};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    Durable,
    Fast,
}

#[derive(Clone, Debug)]
pub struct RecordInput {
    pub kind: Kind,
    pub tenant_id: u32,
    pub content: Vec<u8>,
}

/// Fast acknowledgements expose neither transaction identifiers nor LSNs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Committed {
    Durable { txn: u64, first_lsn: u64, last_lsn: u64 },
    Fast,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Committed,
    NotCommitted,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct WriteError {
    pub kind: Error,
    pub outcome: Outcome,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:?})", self.kind, self.outcome)
    }
}

impl std::error::Error for WriteError {}

pub type WriteResult = std::result::Result<Committed, WriteError>;

fn rejected(kind: Error) -> WriteError {
    WriteError { kind, outcome: Outcome::NotCommitted }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub segment_size: u64,
    pub server_address: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self { segment_size: DEFAULT_SEGMENT_SIZE, server_address: None }
    }
}

#[derive(Debug)]
struct PreparedRecord {
    kind: Kind,
    tenant_id: u32,
    payload: Vec<u8>,
    digest: Digest,
}

/// Caller-side salts, content digests, batch digest, and validation.
#[derive(Debug)]
pub struct PreparedBatch {
    records: Vec<PreparedRecord>,
    digest: Digest,
    mode: Durability,
    size: usize,
}

impl PreparedBatch {
    /// Supplies deterministic salts for simulations, without changing the writer.
    pub fn with_salts(
        batch: Vec<RecordInput>,
        mode: Durability,
        mut salt: impl FnMut() -> Result<[u8; 16]>,
    ) -> std::result::Result<Self, WriteError> {
        let size = batch.iter().try_fold(HEADER_LEN + 44, |total, record| {
            total.checked_add(HEADER_LEN + 16)?.checked_add(record.content.len())
        });
        let size = size.ok_or_else(|| rejected(Error::BatchTooLarge))?;
        if batch.len() > 1024 || size > MAX_FRAME_SIZE {
            return Err(rejected(Error::BatchTooLarge));
        }
        if batch.is_empty()
            || batch.iter().any(|r| {
                r.kind == Kind::Footer || (mode == Durability::Fast && r.kind != Kind::Cache)
            })
        {
            return Err(rejected(Error::InvalidFrame(
                "empty batch or disallowed record kind".into(),
            )));
        }
        let mut records = Vec::with_capacity(batch.len());
        let mut digests = Vec::with_capacity(batch.len());
        for record in batch {
            let salt = salt().map_err(rejected)?;
            let digest = frame::content_digest(&salt, &record.content);
            let mut payload = salt.to_vec();
            payload.extend_from_slice(&record.content);
            digests.push(digest);
            records.push(PreparedRecord {
                kind: record.kind,
                tenant_id: record.tenant_id,
                payload,
                digest,
            });
        }
        Ok(Self { records, digest: frame::batch_digest(&digests), mode, size })
    }
}

#[derive(Debug)]
pub struct WriteRequest {
    batch: PreparedBatch,
    reply: mpsc::Sender<WriteResult>,
}

impl WriteRequest {
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
pub struct WriterCore {
    segments: SegmentWriter,
    clock: Arc<dyn Clock>,
    reads: Arc<ReadState>,
    queue: VecDeque<WriteRequest>,
    chains: BTreeMap<u32, Digest>,
    pending: BTreeMap<u64, (u64, u64)>,
    lsn: u64,
    txn: u64,
    superblock_time: u64,
}

impl std::fmt::Debug for WriterCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterCore").field("lsn", &self.lsn).field("txn", &self.txn).finish()
    }
}

impl WriterCore {
    pub fn open(
        io: Arc<dyn Io>,
        clock: Arc<dyn Clock>,
        path: impl AsRef<Path>,
        opts: Options,
    ) -> Result<(Self, RecoveryReport)> {
        if opts.segment_size < (HEADER_LEN * 2 + 60) as u64
            || usize::try_from(opts.segment_size).is_err()
        {
            return Err(Error::InvalidFrame("segment cannot hold a transaction".into()));
        }
        let mut dir = DataDir::open(io.clone(), path, opts.server_address.as_deref())?;
        let recovered = recover(&mut dir, opts.segment_size)?;
        let reads = Arc::new(ReadState {
            io,
            path: dir.path.clone(),
            index: Mutex::new(recovered.index),
            durable_lsn: AtomicU64::new(recovered.report.durable_lsn),
            poisoned: AtomicBool::new(false),
        });
        let segments = SegmentWriter::new(dir, opts.segment_size, recovered.offset)?;
        let core = Self {
            segments,
            superblock_time: clock.now_ns(),
            clock,
            reads,
            queue: VecDeque::new(),
            chains: recovered.chains,
            pending: BTreeMap::new(),
            lsn: recovered.report.durable_lsn,
            txn: recovered.txn,
        };
        Ok((core, recovered.report))
    }

    pub fn enqueue(&mut self, request: WriteRequest) {
        self.queue.push_back(request);
    }

    pub fn durable_lsn(&self) -> u64 {
        self.reads.durable_lsn.load(Ordering::Acquire)
    }

    pub fn is_poisoned(&self) -> bool {
        self.reads.poisoned.load(Ordering::Acquire)
    }

    pub fn read(&self, lsn: u64) -> Result<Option<Frame>> {
        self.reads.read(lsn)
    }

    fn write(&mut self, batch: PreparedBatch) -> Result<Committed> {
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
        for (i, record) in batch.records.into_iter().enumerate() {
            let prev = if record.kind == Kind::Cache {
                [0; 32]
            } else {
                self.chains.get(&record.tenant_id).copied().unwrap_or([0; 32])
            };
            let header = Header {
                kind: record.kind,
                version: VERSION,
                flags: 0,
                txn,
                lsn: first_lsn + i as u64,
                commit_time_ns: time,
                tenant_id: record.tenant_id,
                tenant_prev_chain: prev,
                content_digest: record.digest,
            };
            if record.kind != Kind::Cache {
                self.chains
                    .insert(record.tenant_id, frame::chain_next(&prev, &header, &record.digest));
            }
            offsets.push((header.lsn, bytes.len() as u64));
            bytes.extend(frame::encode(&Frame { header, payload: record.payload })?);
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
        self.lsn = last_lsn;
        self.txn = txn;
        Ok(Committed::Durable { txn, first_lsn, last_lsn })
    }

    fn publish(&mut self) -> Result<()> {
        lock(&self.reads.index)?.append(&mut self.pending);
        self.reads.durable_lsn.store(self.lsn, Ordering::Release);
        Ok(())
    }

    /// Writes all queued transactions, then syncs all touched files before durable replies.
    pub fn step(&mut self) {
        let mut replies = Vec::new();
        while let Some(request) = self.queue.pop_front() {
            if self.is_poisoned() {
                let _ = request.reply.send(Err(rejected(Error::Poisoned)));
                continue;
            }
            if request.batch.size as u64 > self.segments.capacity() {
                let _ = request.reply.send(Err(rejected(Error::BatchTooLarge)));
                continue;
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
        if self.is_poisoned()
            || now.saturating_sub(self.superblock_time) < 1_000_000_000
            || self.durable_lsn() == self.segments.dir.superblock()
        {
            return Ok(());
        }
        self.superblock_time = now;
        let result = self.segments.dir.install_superblock(self.durable_lsn());
        if result.is_err() {
            self.reads.poisoned.store(true, Ordering::Release);
        }
        result
    }
}

/// Threaded production driver. Dropping the last handle joins the writer and releases its lock.
pub struct Tape {
    sender: Option<mpsc::Sender<WriteRequest>>,
    thread: Option<JoinHandle<()>>,
    reads: Arc<ReadState>,
}

impl std::fmt::Debug for Tape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tape").field("durable_lsn", &self.durable_lsn()).finish()
    }
}

impl Tape {
    pub fn open(
        io: Arc<dyn Io>,
        clock: Arc<dyn Clock>,
        path: impl AsRef<Path>,
        opts: Options,
    ) -> Result<(Self, RecoveryReport)> {
        let (mut core, report) = WriterCore::open(io, clock, path, opts)?;
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
        })?;
        Ok((Self { sender: Some(sender), thread: Some(thread), reads }, report))
    }

    pub fn append(&self, batch: Vec<RecordInput>, mode: Durability) -> WriteResult {
        if self.reads.poisoned.load(Ordering::Acquire) {
            return Err(rejected(Error::Poisoned));
        }
        let batch = PreparedBatch::with_salts(batch, mode, os_salt)?;
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

    pub fn durable_lsn(&self) -> u64 {
        self.reads.durable_lsn.load(Ordering::Acquire)
    }

    pub fn read(&self, lsn: u64) -> Result<Option<Frame>> {
        self.reads.read(lsn)
    }
}

impl Drop for Tape {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
fn os_salt() -> Result<[u8; 16]> {
    use std::io::Read;
    let mut salt = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut salt)?;
    Ok(salt)
}

#[cfg(not(unix))]
fn os_salt() -> Result<[u8; 16]> {
    Err(Error::Io(std::io::ErrorKind::Unsupported, "OS salt source requires Unix".into()))
}
