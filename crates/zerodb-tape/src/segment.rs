//! Inline segment preparation and whole-transaction placement.

use crate::dir::{DataDir, Handle};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

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
        })
    }

    /// Reserves a whole transaction's location before its CRCs are computed.
    pub fn reserve_batch(&mut self, len: usize) -> Result<(u64, u64)> {
        if len as u64 > self.size {
            return Err(Error::BatchTooLarge);
        }
        if len as u64 > self.size - self.offset {
            let padding = self.size - self.offset;
            let id = self.dir.prepare_next_segment(self.size)?;
            let file = Handle::open(self.dir.io.clone(), &self.dir.segment_path(id))?;
            self.files.insert(id, file);
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
        self.files.retain(|id, _| *id == self.active);
        Ok(())
    }

    pub(crate) fn discard_partial(&mut self) -> Result<()> {
        let file = self.files.get(&self.active).ok_or(Error::Poisoned)?;
        self.dir.io.set_len(file.id, self.offset)?;
        self.dir.io.set_len(file.id, self.size)
    }
}
