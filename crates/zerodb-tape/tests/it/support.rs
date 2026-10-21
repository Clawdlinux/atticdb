use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use zerodb_tape::Result;
use zerodb_tape::{Error, FileId, Io, LockGuard, SimDisk};

#[derive(Debug)]
pub struct ObservedIo {
    pub disk: SimDisk,
    pub syncs: AtomicUsize,
    pub segment_bytes: AtomicU64,
    pub segment_syncs: AtomicUsize,
    pub fail_at: AtomicUsize,
    pub slow_ms: AtomicU64,
    pub capture: AtomicBool,
    pub images: Mutex<Vec<SimDisk>>,
    pub handles: Mutex<BTreeMap<FileId, PathBuf>>,
    pub events: Mutex<Vec<String>>,
    pub space_error: AtomicBool,
    pub capacity_after_space_check: Mutex<Option<u64>>,
    pub full_next_create: AtomicBool,
    pub full_manifest_write: AtomicBool,
    pub fail_directory_sync: AtomicBool,
    pub fail_extension: AtomicBool,
    pub sparse_allocation: bool,
}

impl ObservedIo {
    #[track_caller]
    pub fn new(seed: u64) -> Self {
        Self {
            disk: SimDisk::new(seed),
            syncs: AtomicUsize::new(0),
            segment_bytes: AtomicU64::new(0),
            segment_syncs: AtomicUsize::new(0),
            fail_at: AtomicUsize::new(0),
            slow_ms: AtomicU64::new(0),
            capture: AtomicBool::new(false),
            images: Mutex::new(Vec::new()),
            handles: Mutex::new(BTreeMap::new()),
            events: Mutex::new(Vec::new()),
            space_error: AtomicBool::new(false),
            capacity_after_space_check: Mutex::new(None),
            full_next_create: AtomicBool::new(false),
            full_manifest_write: AtomicBool::new(false),
            fail_directory_sync: AtomicBool::new(false),
            fail_extension: AtomicBool::new(false),
            sparse_allocation: false,
        }
    }

    #[track_caller]
    fn event(&self, label: String) {
        self.events.lock().unwrap().push(label);
        if self.capture.load(Ordering::SeqCst) {
            self.images.lock().unwrap().push(self.disk.crash().unwrap());
        }
    }

    #[track_caller]
    fn name(&self, file: FileId) -> PathBuf {
        self.handles.lock().unwrap()[&file].clone()
    }
}

impl Io for ObservedIo {
    fn create(&self, path: &Path) -> Result<FileId> {
        if self.full_next_create.swap(false, Ordering::SeqCst) {
            return Err(Error::Io(
                std::io::ErrorKind::StorageFull,
                "injected create failure".into(),
            ));
        }
        let file = self.disk.create(path)?;
        self.handles.lock().unwrap().insert(file, path.to_owned());
        self.event(format!("create {}", path.display()));
        Ok(file)
    }
    fn open(&self, path: &Path) -> Result<FileId> {
        let file = self.disk.open(path)?;
        self.handles.lock().unwrap().insert(file, path.to_owned());
        Ok(file)
    }
    fn close(&self, file: FileId) -> Result<()> {
        self.disk.close(file)?;
        self.handles.lock().unwrap().remove(&file);
        Ok(())
    }
    fn write_at(&self, file: FileId, off: u64, bytes: &[u8]) -> Result<()> {
        if self.name(file).file_name().unwrap() == "MANIFEST.tmp"
            && self.full_manifest_write.swap(false, Ordering::SeqCst)
        {
            return Err(Error::Io(
                std::io::ErrorKind::StorageFull,
                "injected manifest failure".into(),
            ));
        }
        if self.name(file).file_name().unwrap().to_string_lossy().starts_with("seg-") {
            self.segment_bytes.fetch_add(bytes.len() as u64, Ordering::SeqCst);
        }
        let result = self.disk.write_at(file, off, bytes);
        self.event(format!("write {} {off}", self.name(file).display()));
        result
    }
    fn read_at(&self, file: FileId, off: u64, bytes: &mut [u8]) -> Result<usize> {
        self.disk.read_at(file, off, bytes)
    }
    fn len(&self, file: FileId) -> Result<u64> {
        self.disk.len(file)
    }
    fn set_len(&self, file: FileId, len: u64) -> Result<()> {
        if len > self.disk.len(file)? && self.fail_extension.swap(false, Ordering::SeqCst) {
            return Err(Error::Io(std::io::ErrorKind::Other, "injected extension failure".into()));
        }
        let result = self.disk.set_len(file, len);
        self.event(format!("resize {} {len}", self.name(file).display()));
        result
    }
    fn allocate(&self, file: FileId, len: u64) -> Result<()> {
        let result = if self.sparse_allocation {
            self.disk.set_len(file, len)
        } else {
            self.disk.allocate(file, len)
        };
        self.event(format!("resize {} {len}", self.name(file).display()));
        result
    }
    fn available_bytes(&self, dir: &Path) -> Result<u64> {
        if self.space_error.swap(false, Ordering::SeqCst) {
            return Err(Error::Io(std::io::ErrorKind::Other, "injected statvfs failure".into()));
        }
        let available = self.disk.available_bytes(dir)?;
        if let Some(capacity) = self.capacity_after_space_check.lock().unwrap().take() {
            self.disk.set_capacity(capacity)?;
        }
        Ok(available)
    }
    fn total_bytes(&self, dir: &Path) -> Result<u64> {
        self.disk.total_bytes(dir)
    }
    fn sync_data(&self, file: FileId) -> Result<()> {
        if self.name(file).file_name().unwrap().to_string_lossy().starts_with("seg-") {
            self.segment_syncs.fetch_add(1, Ordering::SeqCst);
        }
        let sync = self.syncs.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_at.load(Ordering::SeqCst) == sync {
            self.disk.fail_next_sync()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(self.slow_ms.load(Ordering::SeqCst)));
        let result = self.disk.sync_data(file);
        self.event(format!("sync {}", self.name(file).display()));
        result
    }
    fn sync_dir(&self, dir: &Path) -> Result<()> {
        if self.fail_directory_sync.swap(false, Ordering::SeqCst) {
            self.disk.fail_next_sync()?;
        }
        let result = self.disk.sync_dir(dir);
        self.event(format!("dir-sync {}", dir.display()));
        result
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let result = self.disk.rename(from, to);
        self.event(format!("rename {} {}", from.display(), to.display()));
        result
    }
    fn remove(&self, path: &Path) -> Result<()> {
        let result = self.disk.remove(path);
        self.event(format!("remove {}", path.display()));
        result
    }
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        self.disk.list(dir)
    }
    fn exists(&self, path: &Path) -> Result<bool> {
        self.disk.exists(path)
    }
    fn try_lock_exclusive(&self, path: &Path) -> Result<LockGuard> {
        self.disk.try_lock_exclusive(path)
    }
}

use std::sync::{mpsc, Arc};
use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
use zerodb_tape::{
    CommittedRecord, Durability, Kind, Prepare, RecordInput, StateMachine, TapeOptions, WriteResult,
};
mod fs {
    const ROOT: &str = env!("CARGO_TARGET_TMPDIR");
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/support.rs"));
}
pub(crate) use fs::TestDir;

#[track_caller]
pub(crate) fn record(kind: Kind, tenant_id: u32, content: impl AsRef<[u8]>) -> RecordInput {
    RecordInput { kind, tenant_id, content: content.as_ref().to_vec() }
}
#[track_caller]
pub(crate) fn submit<S: StateMachine>(
    core: &mut WriterCore<S>,
    records: Vec<RecordInput>,
    mode: Durability,
) -> mpsc::Receiver<WriteResult> {
    let (request, reply) = WriteRequest::new(PreparedBatch::new(records, mode).unwrap());
    core.enqueue(request);
    reply
}
#[track_caller]
pub(crate) fn opts(size: u64) -> TapeOptions {
    TapeOptions { segment_size: size, ..TapeOptions::default() }
}
#[derive(Debug, Default)]
pub(crate) struct Log {
    pub published: Vec<CommittedRecord>,
    pub replayed: Vec<CommittedRecord>,
    pub prepares: usize,
    pub threads: Vec<std::thread::ThreadId>,
}

#[derive(Default)]
pub(crate) enum Behavior {
    #[default]
    Pass,
    Sequence,
    Dedup,
    Resize {
        len: usize,
        on_prepare: Option<usize>,
    },
    InvalidRewrite,
}

#[derive(Default)]
pub(crate) struct Machine {
    pub prepares: Arc<AtomicUsize>,
    pub expected_segments: Option<(Arc<SimDisk>, usize)>,
    pub behavior: Behavior,
    pub log: Arc<Mutex<Log>>,
    pub replies: Arc<Mutex<Vec<mpsc::Receiver<WriteResult>>>>,
    pub fail_replay: bool,
    pub seq: u64,
    pub cached: bool,
}

impl Machine {
    #[track_caller]
    pub(crate) fn new(behavior: Behavior) -> Self {
        Self { behavior, ..Self::default() }
    }
}

impl StateMachine for Machine {
    fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        let mut log = self.log.lock().unwrap();
        log.prepares += 1;
        log.threads.push(std::thread::current().id());
        if let Some((disk, expected)) = &self.expected_segments {
            let count = disk
                .list(Path::new("db"))
                .unwrap()
                .iter()
                .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("seg-"))
                .count();
            assert_eq!(count, *expected, "segments must be reserved before prepare");
        }
        match self.behavior {
            Behavior::Pass => {}
            Behavior::Resize { len, on_prepare } => {
                if on_prepare.is_none_or(|prepare| log.prepares == prepare) {
                    for record in batch.records_mut() {
                        record.content.resize(len, 7);
                    }
                }
            }
            Behavior::InvalidRewrite => {
                let record = batch.records_mut().next().unwrap();
                match record.content.as_slice() {
                    b"footer" => record.kind = Kind::Footer,
                    b"durable" => record.kind = Kind::Turn,
                    _ => record.content = vec![0; 4 * 1024 * 1024],
                }
            }
            Behavior::Sequence => {
                match batch.records().next().unwrap().content.as_slice() {
                    b"reject" => {
                        return Prepare::Reject(zerodb_tape::WriteError {
                            kind: Error::InvalidFrame("rejected".into()),
                            outcome: zerodb_tape::Outcome::NotCommitted,
                        })
                    }
                    b"reply" => return Prepare::Reply(receipt(77, 88)),
                    _ => {}
                }
                for record in batch.records_mut() {
                    self.seq += 1;
                    record.content = self.seq.to_le_bytes().to_vec();
                }
            }
            Behavior::Dedup => {
                if batch.records().next().unwrap().content != b"trailing" {
                    if self.cached {
                        return Prepare::Reply(receipt(1, 1));
                    }
                    self.cached = true;
                }
            }
        }
        Prepare::Write
    }
    fn publish(&mut self, records: &[CommittedRecord]) {
        assert!(
            !matches!(self.behavior, Behavior::InvalidRewrite),
            "invalid rewrite must not publish"
        );
        for reply in self.replies.lock().unwrap().iter() {
            assert!(matches!(reply.try_recv(), Err(mpsc::TryRecvError::Empty)));
        }
        self.log.lock().unwrap().published.extend_from_slice(records);
    }
    fn replay(&mut self, record: &CommittedRecord) -> Result<()> {
        if self.fail_replay {
            return Err(Error::Configuration("replay failed".into()));
        }
        if matches!(self.behavior, Behavior::Sequence) {
            self.seq = u64::from_le_bytes(record.payload.as_ref().try_into().unwrap());
        }
        self.log.lock().unwrap().replayed.push(record.clone());
        Ok(())
    }
}

#[track_caller]
pub(crate) fn receipt(txn: u64, lsn: u64) -> zerodb_tape::Committed {
    zerodb_tape::Committed::Durable { txn, first_lsn: lsn, last_lsn: lsn, reply: Vec::new() }
}

#[track_caller]
pub(crate) fn superblock(io: &ObservedIo, size: u64) -> u64 {
    zerodb_tape::sim::DataDir::open(std::sync::Arc::new(io.disk.crash().unwrap()), "db", None, size)
        .unwrap()
        .superblock()
}

#[track_caller]
pub(crate) fn primed_writer(
    floor: Option<u64>,
    capacity: u64,
    lsns: u64,
) -> (Arc<ObservedIo>, WriterCore<Machine>, Arc<AtomicUsize>) {
    let io = Arc::new(ObservedIo::new(8));
    io.disk.set_capacity(capacity).unwrap();
    let prepares = Arc::new(AtomicUsize::new(0));
    let opts =
        TapeOptions { segment_size: 4096, free_space_floor: floor, ..TapeOptions::default() };
    let (mut core, _) = WriterCore::open_with(
        Machine { prepares: prepares.clone(), ..Machine::default() },
        io.clone(),
        Arc::new(SimClock::default()),
        "db",
        opts,
    )
    .unwrap();
    io.disk.set_capacity(capacity.max(16384)).unwrap();
    let first = submit(&mut core, vec![record(Kind::Turn, 1, vec![7; 3800])], Durability::Durable);
    core.step();
    first.recv().unwrap().unwrap();
    while core.durable_lsn() < lsns {
        let reply = submit(&mut core, vec![record(Kind::Turn, 1, [7; 8])], Durability::Durable);
        core.step();
        reply.recv().unwrap().unwrap();
    }
    io.disk.set_capacity(capacity).unwrap();
    (io, core, prepares)
}

#[track_caller]
pub(crate) fn manifest(disk: &SimDisk) -> Vec<u8> {
    let file = disk.open(Path::new("db/MANIFEST")).unwrap();
    let mut bytes = vec![0; disk.len(file).unwrap() as usize];
    assert_eq!(disk.read_at(file, 0, &mut bytes).unwrap(), bytes.len());
    disk.close(file).unwrap();
    bytes
}

pub(crate) mod wire {
    use std::path::Path;
    use std::sync::Arc;
    use zerodb_tape::sim::DataDir;
    use zerodb_tape::{Io, SimDisk};

    use zerodb_tape::sim::frame::{self, DecodeError, Footer, Frame, Header, Kind, VERSION};

    #[track_caller]
    pub(crate) fn frame(kind: Kind, content: &[u8]) -> Frame {
        Frame {
            header: Header {
                kind,
                version: VERSION,
                flags: 0,
                txn: 11,
                lsn: 22,
                commit_time_ns: 33,
                tenant_id: 44,
            },
            payload: content.to_vec(),
        }
    }

    #[track_caller]
    pub(crate) fn encode(frame: &Frame) -> zerodb_tape::Result<Vec<u8>> {
        frame::encode(frame, 7, 128)
    }

    #[track_caller]
    pub(crate) fn decode(bytes: &[u8]) -> Result<(Frame, usize), DecodeError> {
        frame::decode(bytes, 7, 128)
    }

    #[track_caller]
    pub(crate) fn refresh_crc(bytes: &mut [u8]) {
        let crc = frame::checksum(7, 128, &bytes[4..]);
        bytes[..4].copy_from_slice(&crc.to_le_bytes());
    }

    #[track_caller]
    pub(crate) fn transaction(lsn: u64, txn: u64, location: (u64, u64)) -> (Vec<u8>, (u64, u64)) {
        let header = Header {
            kind: Kind::Turn,
            version: VERSION,
            flags: 0,
            txn,
            lsn,
            commit_time_ns: txn,
            tenant_id: 1,
        };
        let (segment, offset) = location;
        let mut bytes = frame::encode(
            &Frame { header: header.clone(), payload: b"hello".to_vec() },
            segment,
            offset,
        )
        .unwrap();
        let footer = Footer {
            txn,
            first_lsn: lsn,
            synced_lsn: 0,
            record_count: 1,
            records_crc: zerodb_tape::sim::crc32c(&bytes[..4]),
        };
        bytes.extend(
            frame::encode(
                &Frame {
                    header: Header { kind: Kind::Footer, tenant_id: 0, ..header },
                    payload: footer.encode().to_vec(),
                },
                segment,
                offset + bytes.len() as u64,
            )
            .unwrap(),
        );
        let next = (segment, offset + bytes.len() as u64);
        (bytes, next)
    }

    #[track_caller]
    pub(crate) fn setup() -> (Arc<SimDisk>, DataDir) {
        let io = Arc::new(SimDisk::new(1));
        let mut dir = DataDir::open(io.clone(), "db", None, 1024).unwrap();
        dir.prepare_next_segment(1024).unwrap();
        (io, dir)
    }

    #[track_caller]
    pub(crate) fn write(io: &SimDisk, dir: &DataDir, id: u64, offset: u64, bytes: &[u8]) {
        let file = io.open(&dir.segment_path(id)).unwrap();
        io.write_at(file, offset, bytes).unwrap();
        io.sync_data(file).unwrap();
        io.close(file).unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Damage {
        Flip(u64),
        ZeroFooter(u64),
        GarbageFooter,
        MisdirectedFooter(u64, u64),
        InvalidProof(usize),
    }

    #[track_caller]
    pub(crate) fn damage(disk: &SimDisk, size: u64, fault: Damage, case: &str) {
        use zerodb_tape::sim::frame;
        let first = disk.open(Path::new("db/seg-0000000000000001.zdb")).unwrap();
        match fault {
            Damage::Flip(offset) => disk.flip_bit(first, offset).unwrap(),
            Damage::ZeroFooter(offset) => disk.write_at(first, offset, &[0; 72]).unwrap(),
            Damage::GarbageFooter => disk.write_at(first, 48, &[0xff; 72]).unwrap(),
            Damage::MisdirectedFooter(segment, offset) => {
                let file = disk.open(Path::new(&format!("db/seg-{segment:016}.zdb"))).unwrap();
                let mut bytes = [0; 48];
                assert_eq!(disk.read_at(file, offset, &mut bytes).unwrap(), 48, "{case}");
                disk.close(file).unwrap();
                disk.write_at(first, 48, &bytes).unwrap();
            }
            Damage::InvalidProof(field) => {
                let mut bytes = vec![0; size as usize];
                assert_eq!(disk.read_at(first, 0, &mut bytes).unwrap(), bytes.len(), "{case}");
                bytes[40] ^= 1;
                let footer = 168;
                bytes[footer + 40 + field] ^= if field == 16 { 2 } else { 1 };
                let crc = frame::checksum(1, footer as u64, &bytes[footer + 4..footer + 72]);
                bytes[footer..footer + 4].copy_from_slice(&crc.to_le_bytes());
                disk.write_at(first, 0, &bytes).unwrap();
            }
        }
        disk.sync_data(first).unwrap();
        disk.close(first).unwrap();
    }

    #[track_caller]
    pub(crate) fn empty_footer(disk: &SimDisk) {
        use zerodb_tape::sim::frame::{self, Footer, Frame, Header, VERSION};
        let footer = Frame {
            header: Header {
                kind: Kind::Footer,
                version: VERSION,
                flags: 0,
                txn: 1,
                lsn: 1,
                commit_time_ns: 1,
                tenant_id: 0,
            },
            payload: Footer {
                txn: 1,
                first_lsn: 1,
                synced_lsn: 0,
                record_count: 1,
                records_crc: 0,
            }
            .encode()
            .to_vec(),
        };
        let bytes = frame::encode(&footer, 1, 0).unwrap();
        let file = disk.open(Path::new("db/seg-0000000000000001.zdb")).unwrap();
        disk.write_at(file, 0, &bytes).unwrap();
        disk.sync_data(file).unwrap();
        disk.close(file).unwrap();
    }
}

pub(crate) mod storage {
    use std::path::Path;

    use zerodb_tape::{FileId, Io, SimDisk};

    #[track_caller]
    pub(crate) fn bytes(io: &dyn Io, file: FileId) -> Vec<u8> {
        let mut data = vec![0; io.len(file).unwrap() as usize];
        let count = io.read_at(file, 0, &mut data).unwrap();
        assert_eq!(count, data.len());
        data
    }

    #[track_caller]
    pub(crate) fn durable_file(disk: &SimDisk, name: &str, data: &[u8]) -> FileId {
        let file = disk.create(Path::new(name)).unwrap();
        disk.write_at(file, 0, data).unwrap();
        disk.sync_data(file).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
        file
    }
}

#[track_caller]
pub(crate) fn superblock_installs(io: &ObservedIo) -> usize {
    io.events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.as_str() == "rename db/SUPERBLOCK.tmp db/SUPERBLOCK")
        .count()
}

#[cfg(target_os = "linux")]
pub fn preallocation_supported(dir: &Path) -> bool {
    use rustix::io::Errno;
    let path = dir.join("preallocation-probe");
    let file =
        std::fs::OpenOptions::new().create_new(true).read(true).write(true).open(&path).unwrap();
    let allocation =
        rustix::fs::fallocate(&file, rustix::fs::FallocateFlags::empty(), 0, 1024 * 1024);
    let supported = match allocation {
        Ok(()) => true,
        Err(error)
            if error == Errno::OPNOTSUPP || error == Errno::NOTSUP || error == Errno::NOSYS =>
        {
            eprintln!(
                "skipping allocation block check: preallocation unsupported (errno={}): {error}",
                error.raw_os_error()
            );
            false
        }
        Err(error) => panic!("preallocation probe failed: {error}"),
    };
    drop(file);
    std::fs::remove_file(path).unwrap();
    supported
}
