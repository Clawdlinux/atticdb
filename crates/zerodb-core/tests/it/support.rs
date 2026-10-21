use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use zerodb_tape::{Error, FileId, Io, LockGuard, Result, SimDisk};

#[derive(Debug)]
pub struct ProbeIo {
    pub disk: SimDisk,
    pub fail_read: AtomicBool,
    pub space_error: AtomicBool,
    pub allocation_full: AtomicBool,
    files: Mutex<BTreeMap<FileId, PathBuf>>,
    pub writes: Mutex<Vec<(String, PathBuf)>>,
    pub read_gate: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
}
impl ProbeIo {
    #[track_caller]
    pub fn new(seed: u64) -> Self {
        Self {
            disk: SimDisk::new(seed),
            fail_read: AtomicBool::new(false),
            space_error: AtomicBool::new(false),
            allocation_full: AtomicBool::new(false),
            files: Mutex::new(BTreeMap::new()),
            writes: Mutex::new(Vec::new()),
            read_gate: Mutex::new(None),
        }
    }
    #[track_caller]
    fn register(&self, path: &Path, file: Result<FileId>) -> Result<FileId> {
        let id = file?;
        self.files.lock().unwrap().insert(id, path.to_owned());
        Ok(id)
    }
}
impl Io for ProbeIo {
    fn create(&self, p: &Path) -> Result<FileId> {
        self.register(p, self.disk.create(p))
    }
    fn open(&self, p: &Path) -> Result<FileId> {
        self.register(p, self.disk.open(p))
    }
    fn close(&self, f: FileId) -> Result<()> {
        self.files.lock().unwrap().remove(&f);
        self.disk.close(f)
    }
    fn write_at(&self, f: FileId, o: u64, d: &[u8]) -> Result<()> {
        let path = self.files.lock().unwrap()[&f].clone();
        self.writes
            .lock()
            .unwrap()
            .push((std::thread::current().name().unwrap_or("unnamed").into(), path));
        self.disk.write_at(f, o, d)
    }
    fn read_at(&self, f: FileId, o: u64, d: &mut [u8]) -> Result<usize> {
        let checkpoint = self.files.lock().unwrap()[&f]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ckpt-");
        let gate = if checkpoint { self.read_gate.lock().unwrap().take() } else { None };
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        if self.fail_read.swap(false, Ordering::SeqCst) {
            return Err(Error::Io(
                std::io::ErrorKind::Other,
                "injected receipt read failure".into(),
            ));
        }
        self.disk.read_at(f, o, d)
    }
    fn len(&self, f: FileId) -> Result<u64> {
        self.disk.len(f)
    }
    fn set_len(&self, f: FileId, n: u64) -> Result<()> {
        self.disk.set_len(f, n)
    }
    fn allocate(&self, f: FileId, n: u64) -> Result<()> {
        if self.allocation_full.swap(false, Ordering::SeqCst) {
            return Err(Error::Io(
                std::io::ErrorKind::StorageFull,
                "injected allocation failure".into(),
            ));
        }
        self.disk.allocate(f, n)
    }
    fn available_bytes(&self, dir: &Path) -> Result<u64> {
        if self.space_error.load(Ordering::SeqCst) {
            return Err(Error::Io(
                std::io::ErrorKind::Other,
                "injected space query failure".into(),
            ));
        }
        self.disk.available_bytes(dir)
    }
    fn total_bytes(&self, dir: &Path) -> Result<u64> {
        if self.space_error.load(Ordering::SeqCst) {
            return Err(Error::Io(
                std::io::ErrorKind::Other,
                "injected space query failure".into(),
            ));
        }
        self.disk.total_bytes(dir)
    }
    fn sync_data(&self, f: FileId) -> Result<()> {
        self.disk.sync_data(f)
    }
    fn sync_dir(&self, p: &Path) -> Result<()> {
        self.disk.sync_dir(p)
    }
    fn rename(&self, a: &Path, b: &Path) -> Result<()> {
        self.disk.rename(a, b)
    }
    fn remove(&self, p: &Path) -> Result<()> {
        self.disk.remove(p)
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

pub(crate) mod sessions {

    use std::sync::Arc;
    use std::time::Duration;
    use zerodb_core::{Engine, EngineOptions, NewTurn, Role, Scope};
    use zerodb_tape::sim::SimClock;
    use zerodb_tape::{SimDisk, TapeOptions};

    #[track_caller]
    pub(crate) fn scope(session: &str) -> Scope {
        Scope { tenant: "tenant".into(), agent: "agent".into(), session: session.into() }
    }

    #[track_caller]
    pub(crate) fn turn(value: u64) -> NewTurn {
        NewTurn { role: Role::User, content: value.to_le_bytes().to_vec(), meta: vec![] }
    }

    #[track_caller]
    pub(crate) fn options() -> EngineOptions {
        EngineOptions {
            tape: TapeOptions { segment_size: 64 * 1024, ..TapeOptions::default() },
            checkpoint_interval: Duration::from_secs(3600),
            checkpoint_bytes: u64::MAX,
            ..EngineOptions::default()
        }
    }

    #[track_caller]
    pub(crate) fn open(disk: Arc<SimDisk>, opts: EngineOptions) -> Engine {
        Engine::open_with(disk, Arc::new(SimClock::default()), "db", opts).unwrap()
    }
}

pub(crate) mod facts {

    use zerodb_core::fact;

    #[track_caller]
    pub(crate) fn fact() -> fact::Fact {
        fact::Fact {
            key: String::new(),
            content: vec![],
            meta: vec![],
            agent: String::new(),
            source_refs: vec![],
            idem_key: None,
            tombstone: false,
        }
    }
}

pub(crate) mod turns {

    use zerodb_core::turn::{NewTurn, Role};

    #[track_caller]
    pub(crate) fn turn() -> NewTurn {
        NewTurn { role: Role::User, content: b"hello".to_vec(), meta: vec![] }
    }
}

mod fs {
    const ROOT: &str = env!("CARGO_TARGET_TMPDIR");
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/support.rs"));
}
pub(crate) use fs::TestDir;
