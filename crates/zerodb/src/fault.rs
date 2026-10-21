//! Test-only storage that fails one append sync on demand. Built only with the
//! `fault-inject` feature, which release builds never enable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zerodb_tape::{Error, FileId, Io, LockGuard, Result, StdIo};

/// The trigger file, relative to the data directory.
pub const TRIGGER: &str = "FAULT-SYNC-ONCE";

/// Real I/O that fails the next segment sync while `<data_dir>/FAULT-SYNC-ONCE` exists.
///
/// The writer reopens each segment with [`Io::open`] after preparing it with
/// [`Io::create`], so only handles from `open` on `seg-*.zdb` are armed. Segment
/// preparation, checkpoint, index and directory syncs pass through, and the failure
/// always lands on an append. The written bytes stay in the file, as with a real
/// sync failure.
#[derive(Debug)]
pub struct FaultIo {
    inner: StdIo,
    trigger: PathBuf,
    segments: Mutex<BTreeSet<FileId>>,
}

impl FaultIo {
    /// Wraps real I/O for one data directory.
    pub fn new(data_dir: &Path) -> Self {
        Self { inner: StdIo::new(), trigger: data_dir.join(TRIGGER), segments: Mutex::default() }
    }

    fn segments(&self) -> Result<std::sync::MutexGuard<'_, BTreeSet<FileId>>> {
        self.segments
            .lock()
            .map_err(|_| Error::Io(std::io::ErrorKind::Other, "fault mutex poisoned".into()))
    }
}

fn is_segment(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("seg-") && name.ends_with(".zdb"))
}

impl Io for FaultIo {
    fn create(&self, path: &Path) -> Result<FileId> {
        self.inner.create(path)
    }

    fn open(&self, path: &Path) -> Result<FileId> {
        let file = self.inner.open(path)?;
        if is_segment(path) {
            self.segments()?.insert(file);
        }
        Ok(file)
    }

    fn close(&self, file: FileId) -> Result<()> {
        self.segments()?.remove(&file);
        self.inner.close(file)
    }

    fn write_at(&self, file: FileId, off: u64, data: &[u8]) -> Result<()> {
        self.inner.write_at(file, off, data)
    }

    fn read_at(&self, file: FileId, off: u64, data: &mut [u8]) -> Result<usize> {
        self.inner.read_at(file, off, data)
    }

    fn len(&self, file: FileId) -> Result<u64> {
        self.inner.len(file)
    }

    fn set_len(&self, file: FileId, n: u64) -> Result<()> {
        self.inner.set_len(file, n)
    }

    fn allocate(&self, file: FileId, len: u64) -> Result<()> {
        self.inner.allocate(file, len)
    }

    fn available_bytes(&self, dir: &Path) -> Result<u64> {
        self.inner.available_bytes(dir)
    }

    fn total_bytes(&self, dir: &Path) -> Result<u64> {
        self.inner.total_bytes(dir)
    }

    fn sync_data(&self, file: FileId) -> Result<()> {
        // Removing the trigger is the claim, so concurrent syncs fail at most once.
        if self.segments()?.contains(&file) && std::fs::remove_file(&self.trigger).is_ok() {
            return Err(Error::SyncFailed("injected by FAULT-SYNC-ONCE".into()));
        }
        self.inner.sync_data(file)
    }

    fn sync_dir(&self, dir: &Path) -> Result<()> {
        self.inner.sync_dir(dir)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.inner.rename(from, to)
    }

    fn remove(&self, path: &Path) -> Result<()> {
        self.inner.remove(path)
    }

    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        self.inner.list(dir)
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        self.inner.exists(path)
    }

    fn try_lock_exclusive(&self, path: &Path) -> Result<LockGuard> {
        self.inner.try_lock_exclusive(path)
    }
}
