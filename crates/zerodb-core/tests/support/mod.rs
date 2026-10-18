use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use zerodb_tape::{Error, FileId, Io, LockGuard, Result, SimDisk};

#[derive(Debug)]
pub struct ProbeIo {
    disk: SimDisk,
    pub fail_read: AtomicBool,
    files: Mutex<BTreeMap<FileId, PathBuf>>,
    pub writes: Mutex<Vec<(String, PathBuf)>>,
}
impl ProbeIo {
    pub fn new(seed: u64) -> Self {
        Self {
            disk: SimDisk::new(seed),
            fail_read: AtomicBool::new(false),
            files: Mutex::new(BTreeMap::new()),
            writes: Mutex::new(Vec::new()),
        }
    }
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
