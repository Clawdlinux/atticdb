//! Object-safe file operations and replaceable clocks.

use crate::{Error, Result};
use std::collections::BTreeMap;
#[cfg(not(windows))]
use std::fs::TryLockError;
use std::fs::{File, OpenOptions};
use std::io::Read;
#[cfg(not(unix))]
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// A file handle belonging to one I/O backend.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FileId(
    /// Backend-assigned handle value.
    pub u64,
);

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
    /// Owns a backend lock and releases it exactly once when dropped.
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
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
    /// Reserves blocks up to `len` on an empty or shorter file, then sets its length.
    /// Copy-on-write filesystems can still run out later.
    fn allocate(&self, file: FileId, len: u64) -> Result<()>;
    /// Returns bytes available to an unprivileged writer in this directory.
    fn available_bytes(&self, dir: &Path) -> Result<u64>;
    /// Returns the filesystem capacity in bytes.
    fn total_bytes(&self, dir: &Path) -> Result<u64>;
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn preallocation_fallback(error: rustix::io::Errno) -> bool {
    use rustix::io::Errno;
    error == Errno::OPNOTSUPP
        || error == Errno::NOTSUP
        || (cfg!(target_os = "linux") && error == Errno::NOSYS)
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

    /// Unix uses one positional `pwrite` loop. Other platforms seek, then write.
    fn write_at(&self, file: FileId, off: u64, data: &[u8]) -> Result<()> {
        let file = self.file(file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let file = lock(&file)?;
            Ok(file.write_all_at(data, off)?)
        }
        #[cfg(not(unix))]
        {
            let mut file = lock(&file)?;
            file.seek(SeekFrom::Start(off))?;
            Ok(file.write_all(data)?)
        }
    }

    /// Unix uses one positional `pread`. Other platforms seek, then read.
    fn read_at(&self, file: FileId, off: u64, data: &mut [u8]) -> Result<usize> {
        let file = self.file(file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let file = lock(&file)?;
            Ok(file.read_at(data, off)?)
        }
        #[cfg(not(unix))]
        {
            let mut file = lock(&file)?;
            file.seek(SeekFrom::Start(off))?;
            Ok(file.read(data)?)
        }
    }

    fn len(&self, file: FileId) -> Result<u64> {
        Ok(lock(self.file(file)?.as_ref())?.metadata()?.len())
    }

    fn set_len(&self, file: FileId, n: u64) -> Result<()> {
        Ok(lock(self.file(file)?.as_ref())?.set_len(n)?)
    }

    /// Linux and macOS reserve blocks with rustix `fallocate`. Other platforms use `set_len`.
    ///
    /// On macOS, rustix tries contiguous `F_PREALLOCATE`, then noncontiguous allocation, then calls `ftruncate`.
    /// rustix asks macOS for the whole target length past the physical end of file.
    /// So macOS preallocates only when an empty file grows. Growing a non-empty file uses `set_len`.
    /// Zero lengths and unsupported preallocation use `set_len`.
    /// APFS and btrfs can need extra space after preallocation. The writer's floor remains the guard.
    fn allocate(&self, file: FileId, len: u64) -> Result<()> {
        let file = self.file(file)?;
        let file = lock(&file)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            #[cfg(target_os = "linux")]
            let preallocate = len > 0;
            #[cfg(target_os = "macos")]
            let preallocate = {
                let current_len = file.metadata()?.len();
                if len == current_len {
                    return Ok(());
                }
                current_len == 0
            };
            if preallocate {
                match rustix::fs::fallocate(&*file, rustix::fs::FallocateFlags::empty(), 0, len) {
                    Ok(()) => {
                        #[cfg(target_os = "macos")]
                        return Ok(());
                    }
                    Err(error) if preallocation_fallback(error) => {}
                    Err(error) => return Err(std::io::Error::from(error).into()),
                }
            }
        }
        Ok(file.set_len(len)?)
    }

    /// Unix uses `statvfs`. Windows uses `GetDiskFreeSpaceExW`, which honors user quotas.
    fn available_bytes(&self, dir: &Path) -> Result<u64> {
        #[cfg(windows)]
        {
            Ok(atticdb_sys::free_space(dir)?.0)
        }
        #[cfg(not(windows))]
        {
            let stat = rustix::fs::statvfs(dir).map_err(std::io::Error::from)?;
            Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
        }
    }

    fn total_bytes(&self, dir: &Path) -> Result<u64> {
        #[cfg(windows)]
        {
            // With a per-user NTFS quota, this total is the quota, not the volume size.
            // So the free-space floor follows the quota, unlike statvfs on Unix.
            Ok(atticdb_sys::free_space(dir)?.1)
        }
        #[cfg(not(windows))]
        {
            let stat = rustix::fs::statvfs(dir).map_err(std::io::Error::from)?;
            Ok(stat.f_blocks.saturating_mul(stat.f_frsize))
        }
    }

    fn sync_data(&self, file: FileId) -> Result<()> {
        Ok(lock(self.file(file)?.as_ref())?.sync_data()?)
    }

    /// A no-op on Windows. Windows documents no directory fsync.
    /// `File::open` also fails on a directory without `FILE_FLAG_BACKUP_SEMANTICS`.
    /// Renames are durable because `rename` passes `MOVEFILE_WRITE_THROUGH`.
    fn sync_dir(&self, dir: &Path) -> Result<()> {
        #[cfg(windows)]
        {
            let _ = dir;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            Ok(File::open(dir)?.sync_all()?)
        }
    }

    /// Windows uses `MoveFileExW` with write-through, so the rename is on disk when it returns.
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        #[cfg(windows)]
        {
            Ok(atticdb_sys::rename_write_through(from, to)?)
        }
        #[cfg(not(windows))]
        {
            Ok(std::fs::rename(from, to)?)
        }
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

    /// Windows locks a byte far past the end of file, so others can still read the holder.
    /// The guard unlocks before closing, because Windows can release a closed handle's lock late.
    fn try_lock_exclusive(&self, path: &Path) -> Result<LockGuard> {
        let mut file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        #[cfg(windows)]
        {
            if atticdb_sys::try_lock_past_eof(&file)? {
                return Ok(LockGuard::new(move || {
                    // Closing releases the lock anyway, so an unlock error changes nothing.
                    let _ = atticdb_sys::unlock_past_eof(&file);
                    drop(file);
                }));
            }
            let mut holder = String::new();
            file.read_to_string(&mut holder)?;
            Err(Error::DataDirLocked { holder })
        }
        #[cfg(not(windows))]
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
#[cfg(any(test, feature = "sim"))]
#[derive(Debug, Default)]
pub struct SimClock(AtomicU64);

#[cfg(any(test, feature = "sim"))]
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

#[cfg(any(test, feature = "sim"))]
impl Clock for SimClock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    mod fs {
        // Cargo doesn't define CARGO_TARGET_TMPDIR for unit tests.
        const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/tmp");
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/support.rs"));
    }

    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn preallocation_errno_policy_preserves_real_errors() {
        use rustix::io::Errno;
        for error in [Errno::OPNOTSUPP, Errno::NOTSUP] {
            assert!(preallocation_fallback(error));
        }
        assert_eq!(preallocation_fallback(Errno::NOSYS), cfg!(target_os = "linux"));
        for error in [Errno::NOSPC, Errno::IO, Errno::INVAL, Errno::PERM, Errno::INTR] {
            assert!(!preallocation_fallback(error));
        }
        assert_eq!(std::io::Error::from(Errno::NOSPC).kind(), std::io::ErrorKind::StorageFull);
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "slow in debug")]
    fn ten_thousand_atomic_installs_release_open_handles() {
        let dir = fs::TestDir::new();
        let io = StdIo::new();
        let temporary = dir.0.join("MANIFEST.tmp");
        let target = dir.0.join("MANIFEST");
        for _ in 0..10_000 {
            let file = io.create(&temporary).unwrap();
            io.write_at(file, 0, b"manifest").unwrap();
            io.rename(&temporary, &target).unwrap();
            io.close(file).unwrap();
            assert!(lock(&io.files).unwrap().is_empty());
        }
        io.remove(&target).unwrap();
    }
}
