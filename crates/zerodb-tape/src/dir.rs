//! Locked directory metadata and atomic replacements.

use crate::crc32c::crc32c;
use crate::io::{lock, FileId, Io, LockGuard};
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) struct Handle {
    pub io: Arc<dyn Io>,
    pub id: FileId,
}

impl Handle {
    pub fn open(io: Arc<dyn Io>, path: &Path) -> Result<Self> {
        let id = io.open(path)?;
        Ok(Self { io, id })
    }

    pub fn create(io: Arc<dyn Io>, path: &Path) -> Result<Self> {
        let id = io.create(path)?;
        Ok(Self { io, id })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.io.close(self.id);
    }
}

/// Installs bytes using write, file sync, rename, then directory sync.
pub fn atomic_install(io: Arc<dyn Io>, dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    if io.exists(&tmp)? {
        io.remove(&tmp)?;
    }
    let file = Handle::create(io.clone(), &tmp)?;
    io.write_at(file.id, 0, bytes)?;
    io.sync_data(file.id).map_err(|e| Error::SyncFailed(e.to_string()))?;
    io.rename(&tmp, &dir.join(name))?;
    io.sync_dir(dir).map_err(|e| Error::SyncFailed(e.to_string()))
}

/// Keeps one zero-filled `spare.zdb` for the next roll, built as `spare.tmp`.
///
/// Zeroed blocks make each later segment sync a data flush only. Failures retry at the next call.
pub fn make_spare(
    io: Arc<dyn Io>,
    dir: &Path,
    size: u64,
    floor: u64,
    space: &Mutex<()>,
) -> Result<()> {
    let (tmp, spare) = (dir.join("spare.tmp"), dir.join("spare.zdb"));
    let file = {
        // The writer checks and reserves under the same lock, so both never pass on the same bytes.
        let _space = lock(space)?;
        if io.exists(&spare)? {
            return Ok(());
        }
        if io.exists(&tmp)? {
            io.remove(&tmp)?;
        }
        // Leave the active segment and a concurrent roll their room above the floor.
        let free = io.available_bytes(dir)?;
        if free < size.saturating_mul(2).saturating_add(floor) {
            return Ok(());
        }
        let file = Handle::create(io.clone(), &tmp)?;
        // A sparse fallback reserves nothing, so the fill could later cut into the floor.
        match io.allocate(file.id, size).and_then(|()| io.available_bytes(dir)) {
            Ok(left) if left <= free.saturating_sub(size) => file,
            reserved => {
                drop(file);
                io.remove(&tmp)?;
                return reserved.map(|_| ());
            }
        }
    };
    let filled = fill_zeros(io.as_ref(), file.id, size);
    drop(file);
    if let Err(error) = filled.and_then(|()| io.rename(&tmp, &spare)) {
        // Free the space now, not at the next call.
        io.remove(&tmp)?;
        return Err(error);
    }
    io.sync_dir(dir).map_err(|e| Error::SyncFailed(e.to_string()))
}

/// Small synced chunks keep ext4 tape syncs from queuing behind the fill.
fn fill_zeros(io: &dyn Io, file: FileId, size: u64) -> Result<()> {
    let zeros = vec![0; 256 << 10];
    let mut offset = 0;
    while offset < size {
        let count = (size - offset).min(zeros.len() as u64) as usize;
        io.write_at(file, offset, &zeros[..count])?;
        offset += count as u64;
        // A macOS sync is a full device flush, and copy-on-write APFS converts no extents.
        if cfg!(target_os = "linux") || offset == size {
            io.sync_data(file).map_err(|e| Error::SyncFailed(e.to_string()))?;
        }
    }
    Ok(())
}

pub(crate) fn zero_tail(io: &dyn Io, file: FileId, from: u64, size: u64) -> Result<()> {
    io.set_len(file, size)?;
    let zeros = vec![0; 1 << 20];
    let mut offset = from;
    while offset < size {
        let count = (size - offset).min(zeros.len() as u64) as usize;
        io.write_at(file, offset, &zeros[..count])?;
        offset += count as u64;
    }
    Ok(())
}

pub(crate) fn corruption(lsn: u64, segment: u64, offset: u64, reason: impl ToString) -> Error {
    Error::CorruptionDetected { lsn, segment, offset, reason: reason.to_string() }
}

pub(crate) fn encode(magic: &[u8; 8], version: u32, values: &[u64]) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend_from_slice(&version.to_le_bytes());
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&crc32c(&bytes).to_le_bytes());
    bytes
}

pub(crate) fn write_superblock(io: Arc<dyn Io>, path: &Path, durable_lsn: u64) -> Result<()> {
    atomic_install(io, path, "SUPERBLOCK", &encode(b"ZDBSUPER", 1, &[durable_lsn]))
}

pub(crate) fn read(
    io: Arc<dyn Io>,
    path: &Path,
    magic: &[u8; 8],
    version: u32,
) -> Result<Vec<u64>> {
    let file = Handle::open(io.clone(), path)?;
    let len = io.len(file.id)?;
    if !(16..=16 * 1024 * 1024).contains(&len) || (len - 16) % 8 != 0 {
        return Err(corruption(0, 0, 0, "invalid metadata length"));
    }
    let mut bytes = vec![0; len as usize];
    read_exact(io.as_ref(), file.id, 0, &mut bytes)?;
    let end = bytes.len() - 4;
    if &bytes[..8] != magic
        || bytes[8..12] != version.to_le_bytes()
        || bytes[end..] != crc32c(&bytes[..end]).to_le_bytes()
    {
        return Err(corruption(0, 0, 0, "invalid metadata magic, version, or CRC"));
    }
    Ok(bytes[12..end].as_chunks::<8>().0.iter().map(|value| u64::from_le_bytes(*value)).collect())
}

pub(crate) fn read_exact(
    io: &dyn Io,
    file: FileId,
    mut off: u64,
    mut bytes: &mut [u8],
) -> Result<()> {
    while !bytes.is_empty() {
        let count = io.read_at(file, off, bytes)?;
        if count == 0 {
            return Err(Error::Io(std::io::ErrorKind::UnexpectedEof, "short read".into()));
        }
        off += count as u64;
        bytes = &mut bytes[count..];
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum SegmentPrepareError {
    NoSpace(Error),
    Storage(Error),
}

impl SegmentPrepareError {
    fn before_manifest(error: Error) -> Self {
        if matches!(error, Error::Io(std::io::ErrorKind::StorageFull, _)) {
            Self::NoSpace(error)
        } else {
            Self::Storage(error)
        }
    }

    fn into_error(self) -> Error {
        match self {
            Self::NoSpace(error) | Self::Storage(error) => error,
        }
    }
}

impl From<Error> for SegmentPrepareError {
    fn from(error: Error) -> Self {
        Self::Storage(error)
    }
}

/// Metadata protected by the directory's exclusive lock.
pub struct DataDir {
    pub(crate) io: Arc<dyn Io>,
    pub(crate) path: PathBuf,
    segments: Vec<u64>,
    segment_size: u64,
    superblock: u64,
    _lock: LockGuard,
}

impl std::fmt::Debug for DataDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataDir")
            .field("path", &self.path)
            .field("segments", &self.segments)
            .finish()
    }
}

impl DataDir {
    /// Takes the lock before reading or initializing metadata. Real directories must already exist.
    pub fn open(
        io: Arc<dyn Io>,
        path: impl AsRef<Path>,
        address: Option<&str>,
        segment_size: u64,
    ) -> Result<Self> {
        Self::open_inner(io, path.as_ref(), address, Some(segment_size))
    }

    pub(crate) fn open_existing(io: Arc<dyn Io>, path: &Path) -> Result<Self> {
        Self::open_inner(io, path, None, None)
    }

    fn open_inner(
        io: Arc<dyn Io>,
        path: &Path,
        address: Option<&str>,
        configured: Option<u64>,
    ) -> Result<Self> {
        let path = path.to_owned();
        let guard = io.try_lock_exclusive(&path.join("LOCK"))?;
        let segment_size = configured.unwrap_or(1);
        if segment_size == 0 || usize::try_from(segment_size).is_err() {
            return Err(Error::Configuration("invalid segment size".into()));
        }
        let mut dir =
            Self { io, path, segments: Vec::new(), segment_size, superblock: 0, _lock: guard };
        if dir.io.exists(&dir.path.join("MANIFEST"))? {
            let values = read(dir.io.clone(), &dir.path.join("MANIFEST"), b"ZDBMANIF", 2)?;
            let (stored, segments) =
                values.split_first().ok_or_else(|| corruption(0, 0, 0, "missing segment size"))?;
            if *stored == 0 || usize::try_from(*stored).is_err() {
                return Err(corruption(0, 0, 0, "invalid stored segment size"));
            }
            dir.segment_size = *stored;
            if let Some(size) = configured {
                dir.check_segment_size(size)?;
            }
            dir.segments = segments.to_vec();
            if dir.segments.windows(2).any(|ids| ids[0] >= ids[1]) {
                return Err(corruption(0, 0, 0, "unordered manifest"));
            }
            let last = dir.segments.last().copied().unwrap_or(0);
            for path in dir.io.list(&dir.path)? {
                let name = path.file_name().and_then(|name| name.to_str());
                let id = name
                    .and_then(|name| name.strip_prefix("seg-"))
                    .and_then(|name| name.strip_suffix(".zdb"))
                    .and_then(|id| id.parse::<u64>().ok());
                // Never trust a spare from before a crash.
                if id.is_some_and(|id| id > last) || matches!(name, Some("spare.tmp" | "spare.zdb"))
                {
                    dir.io.remove(&path)?;
                }
            }
            // A previous open may have unlinked files but failed its directory sync.
            dir.io.sync_dir(&dir.path).map_err(|e| Error::SyncFailed(e.to_string()))?;
            if !dir.io.exists(&dir.path.join("SUPERBLOCK"))? {
                return Err(corruption(0, 0, 0, "missing superblock"));
            }
            let values = read(dir.io.clone(), &dir.path.join("SUPERBLOCK"), b"ZDBSUPER", 1)?;
            if values.len() != 1 {
                return Err(corruption(0, 0, 0, "invalid superblock length"));
            }
            dir.superblock = values[0];
        } else {
            if configured.is_none() {
                return Err(corruption(0, 0, 0, "missing manifest"));
            }
            if dir.io.list(&dir.path)?.iter().any(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("seg-"))
            }) {
                return Err(corruption(0, 0, 0, "missing manifest for existing segments"));
            }
            if dir.io.exists(&dir.path.join("SUPERBLOCK"))? {
                let values = read(dir.io.clone(), &dir.path.join("SUPERBLOCK"), b"ZDBSUPER", 1)?;
                if values != [0] {
                    return Err(corruption(0, 0, 0, "missing manifest"));
                }
            }
            dir.install_superblock(0)?;
            dir.install_manifest(&[])?;
        }
        let file = Handle::open(dir.io.clone(), &dir.path.join("LOCK"))?;
        let holder = format!("pid={} {}\n", std::process::id(), address.unwrap_or(""));
        dir.io.set_len(file.id, 0)?;
        dir.io.write_at(file.id, 0, holder.as_bytes())?;
        Ok(dir)
    }

    /// Returns the segment size stored in manifest version 2.
    pub fn segment_size(&self) -> u64 {
        self.segment_size
    }

    pub(crate) fn check_segment_size(&self, configured: u64) -> Result<()> {
        if configured != self.segment_size {
            return Err(Error::Configuration(format!(
                "segment size {configured} differs from stored {}",
                self.segment_size
            )));
        }
        Ok(())
    }

    /// Lists prepared segments in scan order.
    pub fn segments(&self) -> &[u64] {
        &self.segments
    }

    /// Returns the last installed durable boundary.
    pub fn superblock(&self) -> u64 {
        self.superblock
    }

    /// Replaces the segment list after its files and names are durable.
    pub fn install_manifest(&mut self, segments: &[u64]) -> Result<()> {
        if segments.windows(2).any(|ids| ids[0] >= ids[1]) {
            return Err(Error::InvalidFrame("unordered manifest".into()));
        }
        let mut values = vec![self.segment_size];
        values.extend_from_slice(segments);
        atomic_install(self.io.clone(), &self.path, "MANIFEST", &encode(b"ZDBMANIF", 2, &values))?;
        self.segments = segments.to_vec();
        Ok(())
    }

    /// Records a previously synced LSN using atomic replacement.
    pub fn install_superblock(&mut self, durable_lsn: u64) -> Result<()> {
        write_superblock(self.io.clone(), &self.path, durable_lsn)?;
        self.superblock = durable_lsn;
        Ok(())
    }

    /// Returns a numbered segment path.
    pub fn segment_path(&self, id: u64) -> PathBuf {
        self.path.join(format!("seg-{id:016}.zdb"))
    }

    /// Takes the spare or preallocates a segment, then persists its name before installing the manifest.
    pub fn prepare_next_segment(&mut self, size: u64) -> Result<u64> {
        self.prepare_segment(size).map_err(SegmentPrepareError::into_error)
    }

    pub(crate) fn prepare_segment(
        &mut self,
        size: u64,
    ) -> std::result::Result<u64, SegmentPrepareError> {
        self.check_segment_size(size)?;
        let mut id = self.segments.last().copied().unwrap_or(0);
        loop {
            id = id.checked_add(1).ok_or_else(|| Error::InvalidFrame("segment overflow".into()))?;
            if !self.io.exists(&self.segment_path(id))? {
                break;
            }
        }
        let path = self.segment_path(id);
        let spare = self.path.join("spare.zdb");
        if self.segments.is_empty() {
            // The first segment is made at open, off the write path, so zero-fill it too.
            // A failed or skipped fill falls back to preallocation below.
            let _ = make_spare(self.io.clone(), &self.path, size, 0, &Mutex::new(()));
        }
        if self.io.exists(&spare)? {
            self.io.rename(&spare, &path).map_err(SegmentPrepareError::before_manifest)?;
        } else {
            let file = Handle::create(self.io.clone(), &path)
                .map_err(SegmentPrepareError::before_manifest)?;
            if let Err(error) = self.io.allocate(file.id, size) {
                drop(file);
                self.io.remove(&path)?;
                return Err(SegmentPrepareError::before_manifest(error));
            }
            self.io.sync_data(file.id).map_err(|e| Error::SyncFailed(e.to_string()))?;
        }
        self.io.sync_dir(&self.path).map_err(|e| Error::SyncFailed(e.to_string()))?;
        let mut segments = self.segments.clone();
        segments.push(id);
        self.install_manifest(&segments)?;
        Ok(id)
    }
}
