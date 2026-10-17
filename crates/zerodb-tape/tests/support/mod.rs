#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use zerodb_tape::Result;
use zerodb_tape::{FileId, Io, LockGuard, SimDisk};

#[derive(Debug)]
pub struct ObservedIo {
    pub disk: SimDisk,
    pub syncs: AtomicUsize,
    pub fail_at: AtomicUsize,
    pub slow_ms: AtomicU64,
    pub capture: AtomicBool,
    pub images: Mutex<Vec<SimDisk>>,
    pub handles: Mutex<BTreeMap<FileId, PathBuf>>,
    pub events: Mutex<Vec<String>>,
}

impl ObservedIo {
    pub fn new(seed: u64) -> Self {
        Self {
            disk: SimDisk::new(seed),
            syncs: AtomicUsize::new(0),
            fail_at: AtomicUsize::new(0),
            slow_ms: AtomicU64::new(0),
            capture: AtomicBool::new(false),
            images: Mutex::new(Vec::new()),
            handles: Mutex::new(BTreeMap::new()),
            events: Mutex::new(Vec::new()),
        }
    }

    fn event(&self, label: String) {
        self.events.lock().unwrap().push(label);
        if self.capture.load(Ordering::SeqCst) {
            self.images.lock().unwrap().push(self.disk.crash().unwrap());
        }
    }

    fn name(&self, file: FileId) -> PathBuf {
        self.handles.lock().unwrap()[&file].clone()
    }
}

impl Io for ObservedIo {
    fn create(&self, path: &Path) -> Result<FileId> {
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
        let result = self.disk.set_len(file, len);
        self.event(format!("resize {} {len}", self.name(file).display()));
        result
    }
    fn sync_data(&self, file: FileId) -> Result<()> {
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
