//! Seeded storage with separate file and directory durability.

use crate::io::{lock, FileId, Io, LockGuard};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[derive(Clone, Debug)]
enum Write {
    Bytes(usize, Vec<u8>),
    Resize(usize),
}

impl Write {
    fn apply(&self, bytes: &mut Vec<u8>) {
        match self {
            Self::Bytes(offset, data) => {
                if !data.is_empty() {
                    bytes.resize(bytes.len().max(offset + data.len()), 0);
                    bytes[*offset..offset + data.len()].copy_from_slice(data);
                }
            }
            Self::Resize(len) => bytes.resize(*len, 0),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SimFile {
    durable: Vec<u8>,
    visible: Vec<u8>,
    pending: Vec<Write>,
}

impl SimFile {
    fn write(&mut self, write: Write) {
        write.apply(&mut self.visible);
        self.pending.push(write);
    }
}

#[derive(Clone, Debug)]
enum DirOp {
    Create(PathBuf, FileId),
    Rename(PathBuf, PathBuf, FileId),
    Remove(PathBuf),
}

impl DirOp {
    fn touches(&self, dir: &Path) -> bool {
        match self {
            Self::Create(path, _) | Self::Remove(path) => path.parent() == Some(dir),
            Self::Rename(from, to, _) => from.parent() == Some(dir) || to.parent() == Some(dir),
        }
    }

    fn apply(&self, entries: &mut BTreeMap<PathBuf, FileId>) {
        match self {
            Self::Create(path, id) => {
                entries.insert(path.clone(), *id);
            }
            Self::Remove(path) => {
                entries.remove(path);
            }
            Self::Rename(from, to, id) => {
                if entries.get(from) == Some(id) {
                    entries.remove(from);
                    entries.insert(to.clone(), *id);
                }
            }
        }
    }
}

#[derive(Debug)]
struct State {
    rng: SplitMix64,
    files: BTreeMap<FileId, SimFile>,
    handles: BTreeMap<FileId, FileId>,
    entries: BTreeMap<PathBuf, FileId>,
    durable_entries: BTreeMap<PathBuf, FileId>,
    pending_entries: Vec<DirOp>,
    locked: BTreeSet<FileId>,
    next: u64,
    fail_sync: bool,
    space: Option<usize>,
}

fn io_error(kind: ErrorKind, message: &str) -> Error {
    Error::Io(kind, message.into())
}

impl State {
    fn file(&mut self, id: FileId) -> Result<&mut SimFile> {
        let inode = self
            .handles
            .get(&id)
            .ok_or_else(|| io_error(ErrorKind::NotFound, "unknown file handle"))?;
        self.files.get_mut(inode).ok_or_else(|| io_error(ErrorKind::NotFound, "file not found"))
    }

    fn handle(&mut self, inode: FileId) -> FileId {
        let handle = FileId(self.next);
        self.next += 1;
        self.handles.insert(handle, inode);
        handle
    }

    fn collect(&mut self) {
        let mut retained = BTreeSet::new();
        retained.extend(self.entries.values());
        retained.extend(self.durable_entries.values());
        retained.extend(self.handles.values());
        retained.extend(&self.locked);
        for op in &self.pending_entries {
            if let DirOp::Create(_, id) | DirOp::Rename(_, _, id) = op {
                retained.insert(id);
            }
        }
        self.files.retain(|id, _| retained.contains(id));
    }

    fn sync_check(&mut self) -> Result<()> {
        if std::mem::take(&mut self.fail_sync) {
            return Err(io_error(ErrorKind::Other, "injected sync failure"));
        }
        Ok(())
    }

    fn create(&mut self, path: &Path) -> Result<FileId> {
        if self.entries.contains_key(path) {
            return Err(io_error(ErrorKind::AlreadyExists, "file already exists"));
        }
        let id = FileId(self.next);
        self.next += 1;
        self.files.insert(id, SimFile::default());
        self.entries.insert(path.to_owned(), id);
        self.pending_entries.push(DirOp::Create(path.to_owned(), id));
        Ok(id)
    }
}

/// An in-memory I/O backend with deterministic crash outcomes.
///
/// Paths are logical names. Parent directories exist implicitly.
/// Clones share live state. A crash returns an independent image without locks.
/// Syncing either side of a cross-directory rename persists both directories.
#[derive(Clone, Debug)]
pub struct SimDisk {
    state: Arc<Mutex<State>>,
}

impl SimDisk {
    /// Creates an empty disk with a SplitMix64 seed.
    pub fn new(seed: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                rng: SplitMix64(seed),
                files: BTreeMap::new(),
                handles: BTreeMap::new(),
                entries: BTreeMap::new(),
                durable_entries: BTreeMap::new(),
                pending_entries: Vec::new(),
                locked: BTreeSet::new(),
                next: 0,
                fail_sync: false,
                space: None,
            })),
        }
    }

    /// Fails the next file or directory sync.
    ///
    /// Failed file syncs doom pending writes. Their cached bytes remain visible, but crash recovery drops them.
    pub fn fail_next_sync(&self) -> Result<()> {
        lock(&self.state)?.fail_sync = true;
        Ok(())
    }

    /// Allows this many more written bytes, then returns `StorageFull`.
    pub fn enospc_after_bytes(&self, bytes: usize) -> Result<()> {
        lock(&self.state)?.space = Some(bytes);
        Ok(())
    }

    /// Flips the low bit of one durable byte.
    pub fn flip_bit(&self, file: FileId, offset: u64) -> Result<()> {
        let mut state = lock(&self.state)?;
        let file = state.file(file)?;
        let offset = position(offset)?;
        let byte = file
            .durable
            .get_mut(offset)
            .ok_or_else(|| io_error(ErrorKind::InvalidInput, "bit flip beyond durable data"))?;
        *byte ^= 1;
        file.visible.clone_from(&file.durable);
        for write in &file.pending {
            write.apply(&mut file.visible);
        }
        Ok(())
    }

    /// Builds a crashed image from durable state and seeded pending operations.
    ///
    /// Pending writes can disappear, reorder, or persist only a prefix.
    /// Directory operations can disappear. Each rename is atomic.
    /// Repeating this call without mutations produces the same image.
    pub fn crash(&self) -> Result<Self> {
        let state = lock(&self.state)?;
        let mut rng = state.rng.clone();
        let mut entries = state.durable_entries.clone();
        for op in &state.pending_entries {
            if rng.below(2) != 0 {
                op.apply(&mut entries);
            }
        }
        let linked: BTreeSet<_> = entries.values().copied().collect();
        let mut files = BTreeMap::new();
        for (id, file) in &state.files {
            if !linked.contains(id) {
                continue;
            }
            let mut durable = file.durable.clone();
            let mut writes = file.pending.clone();
            for i in (1..writes.len()).rev() {
                let j = rng.below(i + 1);
                writes.swap(i, j);
            }
            for write in writes {
                if rng.below(2) == 0 {
                    continue;
                }
                match write {
                    Write::Bytes(offset, mut data) => {
                        let len = rng.below(data.len() + 1);
                        data.truncate(len);
                        Write::Bytes(offset, data).apply(&mut durable);
                    }
                    Write::Resize(len) => durable.resize(len, 0),
                }
            }
            files.insert(*id, SimFile { visible: durable.clone(), durable, pending: Vec::new() });
        }
        let handles = state
            .handles
            .iter()
            .filter(|(_, id)| linked.contains(id))
            .map(|(handle, inode)| (*handle, *inode))
            .collect();
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                rng,
                files,
                handles,
                durable_entries: entries.clone(),
                entries,
                pending_entries: Vec::new(),
                locked: BTreeSet::new(),
                next: state.next,
                fail_sync: false,
                space: None,
            })),
        })
    }
}

fn position(offset: u64) -> Result<usize> {
    usize::try_from(offset).map_err(|_| io_error(ErrorKind::InvalidInput, "offset too large"))
}

impl Io for SimDisk {
    fn create(&self, path: &Path) -> Result<FileId> {
        let mut state = lock(&self.state)?;
        let inode = state.create(path)?;
        Ok(state.handle(inode))
    }

    fn open(&self, path: &Path) -> Result<FileId> {
        let mut state = lock(&self.state)?;
        let inode = state
            .entries
            .get(path)
            .copied()
            .ok_or_else(|| io_error(ErrorKind::NotFound, "file not found"))?;
        Ok(state.handle(inode))
    }

    fn close(&self, file: FileId) -> Result<()> {
        let mut state = lock(&self.state)?;
        state
            .handles
            .remove(&file)
            .ok_or_else(|| io_error(ErrorKind::NotFound, "unknown file handle"))?;
        state.collect();
        Ok(())
    }

    fn write_at(&self, file: FileId, off: u64, data: &[u8]) -> Result<()> {
        let offset = position(off)?;
        offset
            .checked_add(data.len())
            .ok_or_else(|| io_error(ErrorKind::InvalidInput, "write offset overflow"))?;
        let mut state = lock(&self.state)?;
        state.file(file)?;
        let count = state.space.map_or(data.len(), |left| left.min(data.len()));
        if let Some(left) = &mut state.space {
            *left -= count;
        }
        if count > 0 {
            state.file(file)?.write(Write::Bytes(offset, data[..count].to_vec()));
        }
        if count != data.len() {
            return Err(io_error(ErrorKind::StorageFull, "injected partial write"));
        }
        Ok(())
    }

    fn read_at(&self, file: FileId, off: u64, data: &mut [u8]) -> Result<usize> {
        let offset = position(off)?;
        let mut state = lock(&self.state)?;
        let bytes = &state.file(file)?.visible;
        let count = data.len().min(bytes.len().saturating_sub(offset));
        if count > 0 {
            data[..count].copy_from_slice(&bytes[offset..offset + count]);
        }
        Ok(count)
    }

    fn len(&self, file: FileId) -> Result<u64> {
        Ok(lock(&self.state)?.file(file)?.visible.len() as u64)
    }

    fn set_len(&self, file: FileId, n: u64) -> Result<()> {
        lock(&self.state)?.file(file)?.write(Write::Resize(position(n)?));
        Ok(())
    }

    fn sync_data(&self, file: FileId) -> Result<()> {
        let mut state = lock(&self.state)?;
        state.file(file)?;
        if let Err(error) = state.sync_check() {
            // Writeback errors can discard dirty writes without evicting their cached bytes.
            state.file(file)?.pending.clear();
            return Err(error);
        }
        let file = state.file(file)?;
        for write in std::mem::take(&mut file.pending) {
            write.apply(&mut file.durable);
        }
        Ok(())
    }

    fn sync_dir(&self, dir: &Path) -> Result<()> {
        let mut state = lock(&self.state)?;
        state.sync_check()?;
        let mut dirs = BTreeSet::from([dir.to_owned()]);
        loop {
            let count = dirs.len();
            for op in &state.pending_entries {
                if let DirOp::Rename(from, to, _) = op {
                    if dirs.iter().any(|dir| op.touches(dir)) {
                        if let Some(parent) = from.parent() {
                            dirs.insert(parent.to_owned());
                        }
                        if let Some(parent) = to.parent() {
                            dirs.insert(parent.to_owned());
                        }
                    }
                }
            }
            if dirs.len() == count {
                break;
            }
        }
        let mut pending = Vec::new();
        for op in std::mem::take(&mut state.pending_entries) {
            if dirs.iter().any(|dir| op.touches(dir)) {
                op.apply(&mut state.durable_entries);
            } else {
                pending.push(op);
            }
        }
        state.pending_entries = pending;
        state.collect();
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let mut state = lock(&self.state)?;
        let id = state
            .entries
            .get(from)
            .copied()
            .ok_or_else(|| io_error(ErrorKind::NotFound, "rename source not found"))?;
        if from != to {
            let op = DirOp::Rename(from.to_owned(), to.to_owned(), id);
            op.apply(&mut state.entries);
            state.pending_entries.push(op);
            state.collect();
        }
        Ok(())
    }

    fn remove(&self, path: &Path) -> Result<()> {
        let mut state = lock(&self.state)?;
        if state.entries.remove(path).is_none() {
            return Err(io_error(ErrorKind::NotFound, "remove target not found"));
        }
        state.pending_entries.push(DirOp::Remove(path.to_owned()));
        state.collect();
        Ok(())
    }

    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        Ok(lock(&self.state)?
            .entries
            .keys()
            .filter(|path| path.parent() == Some(dir))
            .cloned()
            .collect())
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        Ok(lock(&self.state)?.entries.contains_key(path))
    }

    fn try_lock_exclusive(&self, path: &Path) -> Result<LockGuard> {
        let mut state = lock(&self.state)?;
        let id = match state.entries.get(path) {
            Some(id) => *id,
            None => state.create(path)?,
        };
        if !state.locked.insert(id) {
            let file = state
                .files
                .get(&id)
                .ok_or_else(|| io_error(ErrorKind::NotFound, "lock file not found"))?;
            let holder = String::from_utf8_lossy(&file.visible).into_owned();
            return Err(Error::DataDirLocked { holder });
        }
        let state = self.state.clone();
        Ok(LockGuard::new(move || {
            if let Ok(mut state) = state.lock() {
                state.locked.remove(&id);
                state.collect();
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn closed_unlinked_files_are_collected_after_directory_sync() {
        let disk = SimDisk::new(0);
        let path = Path::new("db/data");
        let file = disk.create(path).unwrap();
        disk.write_at(file, 0, &[7; 1024]).unwrap();
        disk.sync_data(file).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
        disk.remove(path).unwrap();
        disk.close(file).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
        assert!(lock(&disk.state).unwrap().files.is_empty());
        assert!(lock(&disk.crash().unwrap().state).unwrap().files.is_empty());
    }

    #[test]
    fn crash_discards_unlinked_files_even_when_still_open() {
        let disk = SimDisk::new(0);
        let path = Path::new("db/data");
        let file = disk.create(path).unwrap();
        disk.write_at(file, 0, &[7; 1024]).unwrap();
        disk.sync_data(file).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
        disk.remove(path).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
        assert_eq!(disk.len(file).unwrap(), 1024);
        assert!(lock(&disk.crash().unwrap().state).unwrap().files.is_empty());
    }
}
