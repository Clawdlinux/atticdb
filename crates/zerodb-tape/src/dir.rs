//! Locked directory metadata and atomic replacements.

use crate::crc32c::crc32c;
use crate::io::{FileId, Io, LockGuard};
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

pub(crate) fn corruption(lsn: u64, segment: u64, offset: u64, reason: impl ToString) -> Error {
    Error::CorruptionDetected { lsn, segment, offset, reason: reason.to_string() }
}

fn encode(magic: &[u8; 8], values: &[u64]) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&crc32c(&bytes).to_le_bytes());
    bytes
}

fn read(io: Arc<dyn Io>, path: &Path, magic: &[u8; 8]) -> Result<Vec<u64>> {
    let file = Handle::open(io.clone(), path)?;
    let len = io.len(file.id)?;
    if !(16..=16 * 1024 * 1024).contains(&len) || (len - 16) % 8 != 0 {
        return Err(corruption(0, 0, 0, "invalid metadata length"));
    }
    let mut bytes = vec![0; len as usize];
    read_exact(io.as_ref(), file.id, 0, &mut bytes)?;
    let end = bytes.len() - 4;
    if &bytes[..8] != magic
        || bytes[8..12] != 1u32.to_le_bytes()
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

/// Metadata protected by the directory's exclusive lock.
pub struct DataDir {
    pub(crate) io: Arc<dyn Io>,
    pub(crate) path: PathBuf,
    segments: Vec<u64>,
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
    pub fn open(io: Arc<dyn Io>, path: impl AsRef<Path>, address: Option<&str>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let guard = io.try_lock_exclusive(&path.join("LOCK"))?;
        let file = Handle::open(io.clone(), &path.join("LOCK"))?;
        let holder = format!("pid={} {}\n", std::process::id(), address.unwrap_or(""));
        io.set_len(file.id, 0)?;
        io.write_at(file.id, 0, holder.as_bytes())?;
        let mut dir = Self { io, path, segments: Vec::new(), superblock: 0, _lock: guard };
        if dir.io.exists(&dir.path.join("MANIFEST"))? {
            dir.segments = read(dir.io.clone(), &dir.path.join("MANIFEST"), b"ZDBMANIF")?;
            if dir.segments.windows(2).any(|ids| ids[0] >= ids[1]) {
                return Err(corruption(0, 0, 0, "unordered manifest"));
            }
            if !dir.io.exists(&dir.path.join("SUPERBLOCK"))? {
                return Err(corruption(0, 0, 0, "missing superblock"));
            }
            let values = read(dir.io.clone(), &dir.path.join("SUPERBLOCK"), b"ZDBSUPER")?;
            if values.len() != 1 {
                return Err(corruption(0, 0, 0, "invalid superblock length"));
            }
            dir.superblock = values[0];
        } else {
            if dir.io.list(&dir.path)?.iter().any(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("seg-"))
            }) {
                return Err(corruption(0, 0, 0, "missing manifest for existing segments"));
            }
            if dir.io.exists(&dir.path.join("SUPERBLOCK"))? {
                let values = read(dir.io.clone(), &dir.path.join("SUPERBLOCK"), b"ZDBSUPER")?;
                if values != [0] {
                    return Err(corruption(0, 0, 0, "missing manifest"));
                }
            }
            dir.install_superblock(0)?;
            dir.install_manifest(&[])?;
        }
        Ok(dir)
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
        atomic_install(self.io.clone(), &self.path, "MANIFEST", &encode(b"ZDBMANIF", segments))?;
        self.segments = segments.to_vec();
        Ok(())
    }

    /// Records a previously synced LSN using atomic replacement.
    pub fn install_superblock(&mut self, durable_lsn: u64) -> Result<()> {
        atomic_install(
            self.io.clone(),
            &self.path,
            "SUPERBLOCK",
            &encode(b"ZDBSUPER", &[durable_lsn]),
        )?;
        self.superblock = durable_lsn;
        Ok(())
    }

    /// Returns a numbered segment path.
    pub fn segment_path(&self, id: u64) -> PathBuf {
        self.path.join(format!("seg-{id:016}.zdb"))
    }

    /// Preallocates a segment and persists its name before installing the manifest.
    pub fn prepare_next_segment(&mut self, size: u64) -> Result<u64> {
        let mut id = self.segments.last().copied().unwrap_or(0);
        loop {
            id = id.checked_add(1).ok_or_else(|| Error::InvalidFrame("segment overflow".into()))?;
            if !self.io.exists(&self.segment_path(id))? {
                break;
            }
        }
        let file = Handle::create(self.io.clone(), &self.segment_path(id))?;
        self.io.set_len(file.id, size)?;
        self.io.sync_data(file.id).map_err(|e| Error::SyncFailed(e.to_string()))?;
        self.io.sync_dir(&self.path).map_err(|e| Error::SyncFailed(e.to_string()))?;
        let mut segments = self.segments.clone();
        segments.push(id);
        self.install_manifest(&segments)?;
        Ok(id)
    }
}
