//! Inline segment preparation and whole-transaction placement.

use crate::dir::{DataDir, Handle, SegmentPrepareError};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Default preallocated segment length.
pub const DEFAULT_SEGMENT_SIZE: u64 = 64 * 1024 * 1024;

/// Owns segment handles and tracks every file requiring sync.
pub struct SegmentWriter {
    pub(crate) dir: DataDir,
    files: BTreeMap<u64, Handle>,
    dirty: BTreeSet<u64>,
    active: u64,
    offset: u64,
    size: u64,
    written_bytes: u64,
    ready: VecDeque<u64>,
    group_reserved: bool,
}

impl std::fmt::Debug for SegmentWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SegmentWriter")
            .field("active", &self.active)
            .field("offset", &self.offset)
            .field("dirty", &self.dirty)
            .finish()
    }
}

impl SegmentWriter {
    pub(crate) fn capacity(&self) -> u64 {
        self.size
    }

    pub(crate) fn position(&self) -> (u64, u64) {
        (self.active, self.written_bytes)
    }

    pub(crate) fn group_space(&self, sizes: impl Iterator<Item = u64>) -> u64 {
        let mut remaining = self.size - self.offset;
        let mut rolls = 0u64;
        let mut bytes = 0u64;
        for size in sizes {
            bytes = bytes.saturating_add(size);
            if size > remaining {
                rolls += 1;
                remaining = self.size;
            }
            remaining -= size;
        }
        bytes
            .saturating_add(rolls.saturating_sub(self.ready.len() as u64).saturating_mul(self.size))
    }

    pub(crate) fn prepare_group(
        &mut self,
        sizes: impl Iterator<Item = u64>,
    ) -> std::result::Result<(), SegmentPrepareError> {
        let mut remaining = self.size - self.offset;
        let mut rolls = 0;
        for size in sizes {
            if size > remaining {
                rolls += 1;
                remaining = self.size;
            }
            remaining -= size;
        }
        while self.ready.len() < rolls {
            let id = self.dir.prepare_segment(self.size)?;
            let file = Handle::open(self.dir.io.clone(), &self.dir.segment_path(id))?;
            self.files.insert(id, file);
            self.ready.push_back(id);
        }
        self.group_reserved = true;
        Ok(())
    }

    pub(crate) fn finish_group(&mut self) {
        self.group_reserved = false;
    }

    /// Opens the last prepared segment at its recovered transaction boundary.
    pub fn new(mut dir: DataDir, size: u64, offset: u64) -> Result<Self> {
        dir.check_segment_size(size)?;
        if size == 0 || offset > size {
            return Err(Error::InvalidFrame("invalid segment size or offset".into()));
        }
        let active = match dir.segments().last().copied() {
            Some(id) => id,
            None => dir.prepare_next_segment(size)?,
        };
        let file = Handle::open(dir.io.clone(), &dir.segment_path(active))?;
        if dir.io.len(file.id)? != size {
            return Err(Error::InvalidFrame("segment size differs from options".into()));
        }
        Ok(Self {
            dir,
            files: BTreeMap::from([(active, file)]),
            dirty: BTreeSet::new(),
            active,
            offset,
            size,
            written_bytes: 0,
            ready: VecDeque::new(),
            group_reserved: false,
        })
    }

    /// Reserves a whole transaction's location before its CRCs are computed.
    pub fn reserve_batch(&mut self, len: usize) -> Result<(u64, u64)> {
        if len as u64 > self.size {
            return Err(Error::BatchTooLarge);
        }
        if len as u64 > self.size - self.offset {
            let padding = self.size - self.offset;
            let id = if let Some(id) = self.ready.pop_front() {
                id
            } else if self.group_reserved {
                return Err(Error::InvalidFrame("prepared group outgrew reserved segments".into()));
            } else {
                let id = self.dir.prepare_next_segment(self.size)?;
                let file = Handle::open(self.dir.io.clone(), &self.dir.segment_path(id))?;
                self.files.insert(id, file);
                id
            };
            self.active = id;
            self.offset = 0;
            self.written_bytes = self.written_bytes.saturating_add(padding);
        }
        Ok((self.active, self.offset))
    }

    /// Writes a whole transaction at its reserved location.
    pub fn write_batch(&mut self, bytes: &[u8]) -> Result<(u64, u64)> {
        self.reserve_batch(bytes.len())?;
        let file = self.files.get(&self.active).ok_or(Error::Poisoned)?;
        let start = (self.active, self.offset);
        self.dirty.insert(self.active);
        self.dir.io.write_at(file.id, self.offset, bytes)?;
        self.offset += bytes.len() as u64;
        self.written_bytes = self.written_bytes.saturating_add(bytes.len() as u64);
        Ok(start)
    }

    /// Syncs all dirty segments, including segments preceding a roll.
    pub fn sync(&mut self) -> Result<()> {
        for id in &self.dirty {
            let file = self.files.get(id).ok_or(Error::Poisoned)?;
            self.dir.io.sync_data(file.id).map_err(|e| Error::SyncFailed(e.to_string()))?;
        }
        self.dirty.clear();
        self.files.retain(|id, _| *id == self.active || self.ready.contains(id));
        Ok(())
    }

    /// Removes partial bytes and restores the tail. Reservation is best-effort.
    /// macOS `F_PREALLOCATE` reserves `len` past the physical end of file.
    /// Sparse fallback lets the poisoned writer sync and publish its successful prefix.
    pub(crate) fn discard_partial(&mut self) -> Result<()> {
        let file = self.files.get(&self.active).ok_or(Error::Poisoned)?;
        self.dir.io.set_len(file.id, self.offset)?;
        self.dir
            .io
            .allocate(file.id, self.size)
            .or_else(|_| self.dir.io.set_len(file.id, self.size))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{Io, SimDisk};
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn discard_keeps_a_sparse_tail_when_reservation_fails() {
        for lost_space in [false, true] {
            let disk = Arc::new(SimDisk::new(18));
            let dir = DataDir::open(disk.clone(), "db", None, 4096).unwrap();
            let mut writer = SegmentWriter::new(dir, 4096, 0).unwrap();
            writer.write_batch(&[1; 512]).unwrap();
            let used = disk.total_bytes(Path::new("db")).unwrap()
                - disk.available_bytes(Path::new("db")).unwrap();
            disk.set_capacity(used).unwrap();
            disk.enospc_after_bytes(100).unwrap();
            assert!(matches!(
                writer.write_batch(&[2; 512]),
                Err(Error::Io(std::io::ErrorKind::StorageFull, _))
            ));
            if lost_space {
                disk.set_capacity(used - 512).unwrap();
            }
            let result = writer.discard_partial();
            let file = writer.files[&writer.active].id;
            disk.enospc_after_bytes(usize::MAX).unwrap();
            if lost_space {
                result.unwrap();
                assert_eq!(disk.len(file).unwrap(), 4096);
                assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 3072);
                disk.write_at(file, 512, &[3; 3072]).unwrap();
                assert!(matches!(
                    disk.write_at(file, 3584, b"x"),
                    Err(Error::Io(std::io::ErrorKind::StorageFull, _))
                ));
                disk.write_at(file, 0, b"retained").unwrap();
            } else {
                result.unwrap();
                assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 0);
                writer.write_batch(&[3; 512]).unwrap();
                assert_eq!(disk.available_bytes(Path::new("db")).unwrap(), 0);
            }
        }
    }
}
