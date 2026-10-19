//! Seeded storage with separate file and directory durability.

use crate::io::{lock, FileId, Io, LockGuard};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const ALLOCATION_UNIT: usize = 512;

#[derive(Clone, Debug, Default)]
struct Blocks(Vec<std::ops::Range<usize>>);

impl Blocks {
    fn bytes(&self) -> u64 {
        self.0.iter().fold(0u64, |used, range| {
            used.saturating_add((range.len() as u64).saturating_mul(ALLOCATION_UNIT as u64))
        })
    }

    fn required(&self, offset: usize, len: usize) -> u64 {
        if len == 0 {
            return 0;
        }
        let start = offset / ALLOCATION_UNIT;
        let end = (offset + len).div_ceil(ALLOCATION_UNIT);
        let occupied: usize = self
            .0
            .iter()
            .map(|range| range.end.min(end).saturating_sub(range.start.max(start)))
            .sum();
        ((end - start - occupied) as u64).saturating_mul(ALLOCATION_UNIT as u64)
    }

    fn writable(&self, offset: usize, len: usize, free: u64) -> usize {
        if self.required(offset, len) <= free {
            return len;
        }
        let (mut low, mut high) = (0, len);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if self.required(offset, middle) <= free {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        low
    }

    fn reserve(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }
        let mut new = offset / ALLOCATION_UNIT..(offset + len).div_ceil(ALLOCATION_UNIT);
        let mut merged = Vec::new();
        for range in std::mem::take(&mut self.0) {
            if range.end < new.start {
                merged.push(range);
            } else if new.end < range.start {
                merged.push(new);
                new = range;
            } else {
                new.start = new.start.min(range.start);
                new.end = new.end.max(range.end);
            }
        }
        merged.push(new);
        self.0 = merged;
    }

    fn truncate(&mut self, len: usize) {
        let end = len.div_ceil(ALLOCATION_UNIT);
        for range in &mut self.0 {
            range.end = range.end.min(end);
        }
        self.0.retain(|range| range.start < range.end);
    }
}

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
    Allocate(usize),
}

impl Write {
    fn apply(&self, bytes: &mut Vec<u8>, blocks: &mut Blocks) {
        match self {
            Self::Bytes(offset, data) => {
                if !data.is_empty() {
                    bytes.resize(bytes.len().max(offset + data.len()), 0);
                    bytes[*offset..offset + data.len()].copy_from_slice(data);
                    blocks.reserve(*offset, data.len());
                }
            }
            Self::Resize(len) | Self::Allocate(len) => {
                bytes.resize(*len, 0);
                blocks.truncate(*len);
                if matches!(self, Self::Allocate(_)) {
                    blocks.reserve(0, *len);
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SimFile {
    durable: Vec<u8>,
    visible: Vec<u8>,
    durable_blocks: Blocks,
    visible_blocks: Blocks,
    pending: Vec<Write>,
    failed_cache: Vec<std::ops::Range<usize>>,
}

impl SimFile {
    fn write(&mut self, write: Write) {
        let overwritten = match &write {
            Write::Bytes(offset, data) => *offset..offset + data.len(),
            Write::Resize(len) | Write::Allocate(len) => *len..usize::MAX,
        };
        let mut cached = Vec::new();
        for range in std::mem::take(&mut self.failed_cache) {
            if range.start < overwritten.start {
                cached.push(range.start..range.end.min(overwritten.start));
            }
            if range.end > overwritten.end {
                cached.push(range.start.max(overwritten.end)..range.end);
            }
        }
        self.failed_cache = cached;
        write.apply(&mut self.visible, &mut self.visible_blocks);
        self.pending.push(write);
    }
}

fn settle(
    mut writes: Vec<Write>,
    bytes: &mut Vec<u8>,
    blocks: &mut Blocks,
    rng: &mut SplitMix64,
    sector_size: usize,
) {
    for i in (1..writes.len()).rev() {
        let j = rng.below(i + 1);
        writes.swap(i, j);
    }
    for write in writes {
        if rng.below(2) == 0 {
            continue;
        }
        match write {
            Write::Bytes(offset, data) => {
                let mut start = 0;
                while start < data.len() {
                    let count =
                        (sector_size - (offset + start) % sector_size).min(data.len() - start);
                    let kept = match rng.below(3) {
                        0 => 0,
                        1 => count,
                        _ => rng.below(count + 1),
                    };
                    Write::Bytes(offset + start, data[start..start + kept].to_vec())
                        .apply(bytes, blocks);
                    start += count;
                }
            }
            Write::Resize(_) | Write::Allocate(_) => write.apply(bytes, blocks),
        }
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
    directories: BTreeSet<PathBuf>,
    durable_entries: BTreeMap<PathBuf, FileId>,
    pending_entries: Vec<DirOp>,
    locked: BTreeSet<FileId>,
    next: u64,
    fail_sync: bool,
    space: Option<usize>,
    capacity: u64,
    sector_size: usize,
}

fn io_error(kind: ErrorKind, message: &str) -> Error {
    Error::Io(kind, message.into())
}

impl State {
    fn available(&self) -> u64 {
        let used = self
            .files
            .values()
            .fold(0u64, |used, file| used.saturating_add(file.visible_blocks.bytes()));
        self.capacity.saturating_sub(used)
    }

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
        for parent in path.ancestors().skip(1) {
            self.directories.insert(parent.to_owned());
        }
        self.pending_entries.push(DirOp::Create(path.to_owned(), id));
        Ok(id)
    }
}

/// An in-memory I/O backend with deterministic crash outcomes.
///
/// Paths are logical names. Parent directories exist implicitly.
/// Clones share live state. A crash returns an independent image without locks.
/// Syncing either side of a cross-directory rename persists both directories.
/// Capacity charges reserved or written 512-byte blocks. Extending with `set_len` stays sparse.
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
                directories: BTreeSet::from([PathBuf::from(".")]),
                durable_entries: BTreeMap::new(),
                pending_entries: Vec::new(),
                locked: BTreeSet::new(),
                next: 0,
                fail_sync: false,
                space: None,
                capacity: 1 << 40,
                sector_size: 512,
            })),
        }
    }

    /// Fails the next file or directory sync.
    ///
    /// Failed file syncs settle pending writes once. Cached bytes remain visible without later writeback.
    pub fn fail_next_sync(&self) -> Result<()> {
        lock(&self.state)?.fail_sync = true;
        Ok(())
    }

    /// Allows this many more written bytes, then returns `StorageFull`.
    pub fn enospc_after_bytes(&self, bytes: usize) -> Result<()> {
        lock(&self.state)?.space = Some(bytes);
        Ok(())
    }

    /// Changes total capacity. Raising it simulates space freed by another process.
    pub fn set_capacity(&self, bytes: u64) -> Result<()> {
        lock(&self.state)?.capacity = bytes;
        Ok(())
    }

    /// Sets the sector size used for crash tearing. The default is 512 bytes.
    ///
    /// Returns an error for zero bytes.
    pub fn set_sector_size(&self, bytes: usize) -> Result<()> {
        if bytes == 0 {
            return Err(io_error(ErrorKind::InvalidInput, "sector size must be positive"));
        }
        lock(&self.state)?.sector_size = bytes;
        Ok(())
    }

    /// Flips the low bit of one durable byte.
    pub fn flip_bit(&self, file: FileId, offset: u64) -> Result<()> {
        self.flip_bit_at(file, offset, 0)
    }

    /// Flips bit 0 through 7 of a durable byte. Rejects larger bit indices.
    pub fn flip_bit_at(&self, file: FileId, offset: u64, bit: u8) -> Result<()> {
        if bit >= 8 {
            return Err(io_error(ErrorKind::InvalidInput, "bit index must be below 8"));
        }
        let mask = 1 << bit;
        let mut state = lock(&self.state)?;
        let file = state.file(file)?;
        let offset = position(offset)?;
        let byte = file
            .durable
            .get_mut(offset)
            .ok_or_else(|| io_error(ErrorKind::InvalidInput, "bit flip beyond durable data"))?;
        *byte ^= mask;
        let shadowed = file.failed_cache.iter().any(|range| range.contains(&offset))
            || file.pending.iter().any(|write| match write {
                Write::Bytes(start, data) => (*start..start + data.len()).contains(&offset),
                Write::Resize(len) | Write::Allocate(len) => *len <= offset,
            });
        if !shadowed {
            if let Some(byte) = file.visible.get_mut(offset) {
                *byte ^= mask;
            }
        }
        Ok(())
    }

    /// Builds a crashed image from durable state and seeded pending operations.
    ///
    /// Pending writes can disappear or reorder. Their sectors persist independently and can tear.
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
            let mut blocks = file.durable_blocks.clone();
            settle(file.pending.clone(), &mut durable, &mut blocks, &mut rng, state.sector_size);
            files.insert(
                *id,
                SimFile {
                    visible: durable.clone(),
                    durable,
                    visible_blocks: blocks.clone(),
                    durable_blocks: blocks,
                    ..SimFile::default()
                },
            );
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
                directories: state.directories.clone(),
                pending_entries: Vec::new(),
                locked: BTreeSet::new(),
                next: state.next,
                fail_sync: false,
                space: None,
                capacity: state.capacity,
                sector_size: state.sector_size,
            })),
        })
    }
}

fn position(offset: u64) -> Result<usize> {
    usize::try_from(offset).map_err(|_| io_error(ErrorKind::InvalidInput, "offset too large"))
}

fn relative_to<'a>(path: &'a Path, dir: &Path) -> Option<&'a Path> {
    if dir == Path::new(".") && path.is_relative() {
        Some(path)
    } else {
        path.strip_prefix(dir).ok()
    }
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
        let free = state.available();
        let capacity_count = state.file(file)?.visible_blocks.writable(offset, data.len(), free);
        let count = state.space.map_or(capacity_count, |left| left.min(capacity_count));
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

    fn allocate(&self, file: FileId, len: u64) -> Result<()> {
        let len = position(len)?;
        let mut state = lock(&self.state)?;
        let required = state.file(file)?.visible_blocks.required(0, len);
        if required > state.available() {
            return Err(io_error(ErrorKind::StorageFull, "allocation exceeds capacity"));
        }
        state.file(file)?.write(Write::Allocate(len));
        Ok(())
    }

    fn available_bytes(&self, _dir: &Path) -> Result<u64> {
        Ok(lock(&self.state)?.available())
    }

    fn total_bytes(&self, _dir: &Path) -> Result<u64> {
        Ok(lock(&self.state)?.capacity)
    }

    fn sync_data(&self, file: FileId) -> Result<()> {
        let mut state = lock(&self.state)?;
        state.file(file)?;
        if let Err(error) = state.sync_check() {
            let mut rng = state.rng.clone();
            let sector_size = state.sector_size;
            let file = state.file(file)?;
            for write in &file.pending {
                file.failed_cache.push(match write {
                    Write::Bytes(offset, data) => *offset..offset + data.len(),
                    Write::Resize(_) | Write::Allocate(_) => {
                        0..file.visible.len().max(file.durable.len())
                    }
                });
            }
            settle(
                std::mem::take(&mut file.pending),
                &mut file.durable,
                &mut file.durable_blocks,
                &mut rng,
                sector_size,
            );
            state.rng = rng;
            return Err(error);
        }
        let file = state.file(file)?;
        for write in std::mem::take(&mut file.pending) {
            write.apply(&mut file.durable, &mut file.durable_blocks);
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
        let state = lock(&self.state)?;
        if state.entries.contains_key(dir) {
            return Err(io_error(ErrorKind::NotADirectory, "list target is a file"));
        }
        let children: BTreeSet<_> = state
            .entries
            .keys()
            .filter_map(|path| relative_to(path, dir))
            .filter_map(|relative| relative.components().next())
            .map(|component| dir.join(component.as_os_str()))
            .collect();
        if children.is_empty() && !state.directories.contains(dir) {
            return Err(io_error(ErrorKind::NotFound, "directory not found"));
        }
        Ok(children.into_iter().collect())
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        let state = lock(&self.state)?;
        Ok(state.directories.contains(path)
            || state.entries.keys().any(|entry| relative_to(entry, path).is_some()))
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
    fn implicit_directory_stays_listable_when_empty() {
        let disk = SimDisk::new(0);
        assert!(disk.list(Path::new(".")).unwrap().is_empty());
        let file = disk.create(Path::new("db/file")).unwrap();
        disk.close(file).unwrap();
        disk.remove(Path::new("db/file")).unwrap();
        assert!(disk.list(Path::new("db")).unwrap().is_empty());
    }

    #[test]
    fn bit_flip_preserves_newer_cached_writes_after_failed_sync() {
        let disk = SimDisk::new(1);
        let file = disk.create(Path::new("db/file")).unwrap();
        disk.write_at(file, 0, b"oldold").unwrap();
        disk.sync_data(file).unwrap();
        disk.write_at(file, 0, b"bad").unwrap();
        disk.fail_next_sync().unwrap();
        assert!(disk.sync_data(file).is_err());
        disk.write_at(file, 0, b"NEW").unwrap();
        disk.sync_data(file).unwrap();
        disk.flip_bit(file, 5).unwrap();
        let mut bytes = [0; 6];
        disk.read_at(file, 0, &mut bytes).unwrap();
        assert_eq!(&bytes[..3], b"NEW");
        assert_eq!(bytes[5], b'd' ^ 1);
    }

    #[test]
    fn failed_write_fates_cannot_overwrite_later_synced_data() {
        for seed in 0..1000 {
            let disk = SimDisk::new(seed);
            let file = disk.create(Path::new("db/file")).unwrap();
            disk.write_at(file, 0, &[0; 1024]).unwrap();
            disk.sync_data(file).unwrap();
            disk.sync_dir(Path::new("db")).unwrap();
            disk.set_len(file, 16).unwrap();
            disk.write_at(file, 0, &[9; 1024]).unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err());
            disk.set_len(file, 1024).unwrap();
            disk.write_at(file, 0, &[7; 1024]).unwrap();
            disk.sync_data(file).unwrap();
            let crash = disk.crash().unwrap();
            let mut bytes = [0; 1024];
            assert_eq!(crash.read_at(file, 0, &mut bytes).unwrap(), 1024, "seed={seed}");
            assert_eq!(bytes, [7; 1024], "seed={seed}");
        }
    }

    #[test]
    fn failed_sync_settles_torn_bytes_without_retry_healing() {
        let mut survived = false;
        let mut lost = false;
        let mut torn = false;
        for seed in 0..200 {
            let disk = SimDisk::new(seed);
            let file = disk.create(Path::new("db/file")).unwrap();
            disk.write_at(file, 0, &[0; 1024]).unwrap();
            disk.sync_data(file).unwrap();
            disk.sync_dir(Path::new("db")).unwrap();
            disk.write_at(file, 0, &[9; 1024]).unwrap();
            disk.fail_next_sync().unwrap();
            assert!(disk.sync_data(file).is_err());
            let settled = lock(&disk.state).unwrap().file(file).unwrap().durable.clone();
            assert!(lock(&disk.state).unwrap().file(file).unwrap().pending.is_empty());
            disk.sync_data(file).unwrap();
            assert_eq!(lock(&disk.state).unwrap().file(file).unwrap().durable, settled);
            disk.flip_bit(file, 1023).unwrap();
            let mut live = [0; 1024];
            disk.read_at(file, 0, &mut live).unwrap();
            assert_eq!(live, [9; 1024]);
            let crash = disk.crash().unwrap();
            let mut bytes = [0; 1024];
            crash.read_at(file, 0, &mut bytes).unwrap();
            assert_eq!(bytes.as_slice(), lock(&disk.state).unwrap().file(file).unwrap().durable);
            survived |= bytes.contains(&9);
            lost |= !bytes.contains(&9);
            torn |= bytes.contains(&9) && bytes.contains(&0);
        }
        assert!(survived && lost && torn);
    }

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
