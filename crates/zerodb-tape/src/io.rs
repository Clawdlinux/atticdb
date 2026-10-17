//! Object-safe file operations and replaceable clocks.

use crate::{Error, Result};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// A file handle belonging to one I/O backend.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FileId(pub u64);

/// An exclusive file lock released when dropped.
pub struct LockGuard {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard").finish_non_exhaustive()
    }
}

impl LockGuard {
    pub(crate) fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self { release: Some(Box::new(release)) }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// File operations shared by real storage and deterministic simulation.
///
/// Paths identify files. Directories must already exist on real storage.
/// Writes complete fully or return an error, possibly after a partial write.
/// Handles remain valid across rename and remove until explicitly closed.
pub trait Io: Send + Sync {
    /// Creates a new empty file, failing if its name already exists.
    fn create(&self, path: &Path) -> Result<FileId>;
    /// Opens an existing file for reading and writing.
    fn open(&self, path: &Path) -> Result<FileId>;
    /// Releases a handle. Further operations on it fail.
    fn close(&self, file: FileId) -> Result<()>;
    /// Writes all bytes at an absolute offset.
    fn write_at(&self, file: FileId, off: u64, data: &[u8]) -> Result<()>;
    /// Reads up to the buffer length at an absolute offset.
    fn read_at(&self, file: FileId, off: u64, data: &mut [u8]) -> Result<usize>;
    /// Returns the current file length.
    fn len(&self, file: FileId) -> Result<u64>;
    /// Truncates or zero-extends a file.
    fn set_len(&self, file: FileId, n: u64) -> Result<()>;
    /// Persists file data and its length, but not its directory entry.
    fn sync_data(&self, file: FileId) -> Result<()>;
    /// Persists directory entries.
    fn sync_dir(&self, dir: &Path) -> Result<()>;
    /// Atomically renames a file, replacing its destination if present.
    fn rename(&self, from: &Path, to: &Path) -> Result<()>;
    /// Removes a file's directory entry.
    fn remove(&self, path: &Path) -> Result<()>;
    /// Lists immediate children as sorted full paths.
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>>;
    /// Checks whether a path exists, preserving errors other than not found.
    fn exists(&self, path: &Path) -> Result<bool>;
    /// Creates or opens a lock file and attempts an exclusive lock.
    fn try_lock_exclusive(&self, path: &Path) -> Result<LockGuard>;
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| Error::Io(std::io::ErrorKind::Other, "I/O mutex poisoned".into()))
}

/// Real filesystem I/O with serialized access to each handle.
#[derive(Debug, Default)]
pub struct StdIo {
    files: Mutex<BTreeMap<FileId, Arc<Mutex<File>>>>,
    next: AtomicU64,
}

impl StdIo {
    /// Creates an empty handle registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self, file: File) -> Result<FileId> {
        let id = FileId(self.next.fetch_add(1, Ordering::Relaxed));
        lock(&self.files)?.insert(id, Arc::new(Mutex::new(file)));
        Ok(id)
    }

    fn file(&self, id: FileId) -> Result<Arc<Mutex<File>>> {
        lock(&self.files)?
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Io(std::io::ErrorKind::NotFound, "unknown file handle".into()))
    }
}

impl Io for StdIo {
    fn create(&self, path: &Path) -> Result<FileId> {
        self.register(OpenOptions::new().read(true).write(true).create_new(true).open(path)?)
    }

    fn open(&self, path: &Path) -> Result<FileId> {
        self.register(OpenOptions::new().read(true).write(true).open(path)?)
    }

    fn close(&self, file: FileId) -> Result<()> {
        lock(&self.files)?
            .remove(&file)
            .ok_or_else(|| Error::Io(std::io::ErrorKind::NotFound, "unknown file handle".into()))?;
        Ok(())
    }

    fn write_at(&self, file: FileId, off: u64, data: &[u8]) -> Result<()> {
        let file = self.file(file)?;
        let mut file = lock(&file)?;
        file.seek(SeekFrom::Start(off))?;
        Ok(file.write_all(data)?)
    }

    fn read_at(&self, file: FileId, off: u64, data: &mut [u8]) -> Result<usize> {
        let file = self.file(file)?;
        let mut file = lock(&file)?;
        file.seek(SeekFrom::Start(off))?;
        Ok(file.read(data)?)
    }

    fn len(&self, file: FileId) -> Result<u64> {
        Ok(lock(self.file(file)?.as_ref())?.metadata()?.len())
    }

    fn set_len(&self, file: FileId, n: u64) -> Result<()> {
        Ok(lock(self.file(file)?.as_ref())?.set_len(n)?)
    }

    fn sync_data(&self, file: FileId) -> Result<()> {
        Ok(lock(self.file(file)?.as_ref())?.sync_data()?)
    }

    fn sync_dir(&self, dir: &Path) -> Result<()> {
        Ok(File::open(dir)?.sync_all()?)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        Ok(std::fs::rename(from, to)?)
    }

    fn remove(&self, path: &Path) -> Result<()> {
        Ok(std::fs::remove_file(path)?)
    }

    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut paths = std::fs::read_dir(dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        Ok(paths)
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        Ok(path.try_exists()?)
    }

    fn try_lock_exclusive(&self, path: &Path) -> Result<LockGuard> {
        let mut file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(LockGuard::new(move || drop(file))),
            Err(TryLockError::WouldBlock) => {
                let mut holder = String::new();
                file.read_to_string(&mut holder)?;
                Err(Error::DataDirLocked { holder })
            }
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

/// A source of nanoseconds since the Unix epoch.
pub trait Clock: Send + Sync {
    /// Returns the current timestamp.
    fn now_ns(&self) -> u64;
}

/// The host wall clock. It can move backward after clock adjustments.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ns(&self) -> u64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_nanos().min(u128::from(u64::MAX)) as u64,
            Err(_) => 0,
        }
    }
}

/// A manually advanced clock, suitable for sharing through an `Arc`.
#[derive(Debug, Default)]
pub struct SimClock(AtomicU64);

impl SimClock {
    /// Creates a clock at a chosen timestamp.
    pub const fn new(now_ns: u64) -> Self {
        Self(AtomicU64::new(now_ns))
    }

    /// Advances time, saturating at the largest timestamp.
    pub fn advance(&self, ns: u64) {
        let mut now = self.0.load(Ordering::SeqCst);
        loop {
            match self.0.compare_exchange(
                now,
                now.saturating_add(ns),
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => now = current,
            }
        }
    }
}

impl Clock for SimClock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn ten_thousand_atomic_installs_release_open_handles() {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("install-handles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let io = StdIo::new();
        let temporary = dir.join("MANIFEST.tmp");
        let target = dir.join("MANIFEST");
        for _ in 0..10_000 {
            let file = io.create(&temporary).unwrap();
            io.write_at(file, 0, b"manifest").unwrap();
            io.rename(&temporary, &target).unwrap();
            io.close(file).unwrap();
            assert!(lock(&io.files).unwrap().is_empty());
        }
        io.remove(&target).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
