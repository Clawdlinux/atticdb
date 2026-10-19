//! Paged per-segment offset arrays. Both offsets and their routing directory have bounded caches.

use crate::dir::{read_exact, Handle};
use crate::{Error, Io, Result};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

const PAGE_BYTES: usize = 4096;
// Charge page storage and cache bookkeeping together.
const PAGE_CHARGE: usize = PAGE_BYTES + 128;
pub(crate) const DEFAULT_BUDGET: usize = 8 * 1024 * 1024;
type PageKey = (bool, u64);

/// Resident tape-index memory and backing-page activity.
#[derive(Clone, Copy, Debug, Default)]
pub struct LsnIndexStats {
    /// Charged resident bytes, including page bookkeeping.
    pub resident_bytes: usize,
    /// Configured resident-memory limit.
    pub budget_bytes: usize,
    /// Pages reloaded from derived backing files.
    pub page_reloads: u64,
    /// Pages successfully removed from the resident cache.
    pub page_evictions: u64,
    /// Evicted dirty pages written to derived backing files.
    pub dirty_page_evictions: u64,
}

struct Page {
    bytes: Box<[u8; PAGE_BYTES]>,
    dirty: bool,
}

pub(crate) struct LsnIndex {
    io: Arc<dyn Io>,
    offsets: Handle,
    routes: Handle,
    pages: HashMap<PageKey, Page>,
    fifo: VecDeque<PageKey>,
    budget: usize,
    len: u64,
    active: Option<(u64, u64)>,
    reloads: u64,
    evictions: u64,
    dirty_evictions: u64,
}

impl std::fmt::Debug for LsnIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LsnIndex")
            .field("records", &self.len)
            .field("stats", &self.stats())
            .finish()
    }
}

impl LsnIndex {
    pub(crate) fn new(io: Arc<dyn Io>, dir: &Path, budget: usize) -> Result<Self> {
        if budget < PAGE_CHARGE {
            return Err(Error::Configuration(format!(
                "LSN index budget must cover {PAGE_CHARGE} bytes"
            )));
        }
        let create = |name| {
            let path = dir.join(name);
            if io.exists(&path)? {
                io.remove(&path)?;
            }
            Handle::create(io.clone(), &path)
        };
        let offsets = create("LSN-OFFSETS")?;
        let routes = create("LSN-ROUTES")?;
        Ok(Self {
            io,
            offsets,
            routes,
            pages: HashMap::new(),
            fifo: VecDeque::new(),
            budget,
            len: 0,
            active: None,
            reloads: 0,
            evictions: 0,
            dirty_evictions: 0,
        })
    }

    pub(crate) fn stats(&self) -> LsnIndexStats {
        LsnIndexStats {
            resident_bytes: self.pages.len() * PAGE_CHARGE,
            budget_bytes: self.budget,
            page_reloads: self.reloads,
            page_evictions: self.evictions,
            dirty_page_evictions: self.dirty_evictions,
        }
    }

    fn page(&mut self, key: PageKey) -> Result<&mut Page> {
        if !self.pages.contains_key(&key) {
            if (self.pages.len() + 1) * PAGE_CHARGE > self.budget {
                let oldest = self.fifo.pop_front().ok_or(Error::Poisoned)?;
                let page = self.pages.remove(&oldest).ok_or(Error::Poisoned)?;
                if page.dirty {
                    let file = if oldest.0 { &self.routes } else { &self.offsets };
                    if let Err(error) = self.io.write_at(
                        file.id,
                        oldest.1 * PAGE_BYTES as u64,
                        page.bytes.as_slice(),
                    ) {
                        self.pages.insert(oldest, page);
                        self.fifo.push_front(oldest);
                        return Err(error);
                    }
                    self.dirty_evictions += 1;
                }
                self.evictions += 1;
            }
            let mut page = Page { bytes: Box::new([0; PAGE_BYTES]), dirty: false };
            let file = if key.0 { &self.routes } else { &self.offsets };
            let offset = key.1 * PAGE_BYTES as u64;
            if offset < self.io.len(file.id)? {
                read_exact(self.io.as_ref(), file.id, offset, page.bytes.as_mut_slice())?;
                self.reloads += 1;
            }
            self.pages.insert(key, page);
            self.fifo.push_back(key);
        }
        self.pages.get_mut(&key).ok_or(Error::Poisoned)
    }

    fn address(lsn: u64, width: u64) -> Result<(u64, usize)> {
        let offset = (lsn - 1).checked_mul(width).ok_or(Error::Rejected("lsn_index"))?;
        Ok((offset / PAGE_BYTES as u64, (offset % PAGE_BYTES as u64) as usize))
    }

    pub(crate) fn insert(&mut self, lsn: u64, segment: u64, offset: u64) -> Result<()> {
        if self.len.checked_add(1) != Some(lsn) {
            return Err(Error::InvalidFrame("non-dense LSN index".into()));
        }
        let first = match self.active {
            Some((id, first)) if id == segment => first,
            Some((id, _)) if id >= segment => {
                return Err(Error::InvalidFrame("unordered index segments".into()))
            }
            _ => lsn,
        };
        let (page, at) = Self::address(lsn, 16)?;
        let route = self.page((true, page))?;
        route.bytes[at..at + 8].copy_from_slice(&segment.to_le_bytes());
        route.bytes[at + 8..at + 16].copy_from_slice(&first.to_le_bytes());
        route.dirty = true;
        let (page, at) = Self::address(lsn, 8)?;
        let offsets = self.page((false, page))?;
        offsets.bytes[at..at + 8].copy_from_slice(&offset.to_le_bytes());
        offsets.dirty = true;
        self.active = Some((segment, first));
        self.len = lsn;
        Ok(())
    }

    pub(crate) fn get(&mut self, lsn: u64) -> Result<Option<(u64, u64)>> {
        if lsn == 0 || lsn > self.len {
            return Ok(None);
        }
        let (page, at) = Self::address(lsn, 16)?;
        let route = self.page((true, page))?;
        let segment =
            u64::from_le_bytes(route.bytes[at..at + 8].try_into().map_err(|_| Error::Poisoned)?);
        let first = u64::from_le_bytes(
            route.bytes[at + 8..at + 16].try_into().map_err(|_| Error::Poisoned)?,
        );
        if first == 0 || first > lsn {
            return Err(Error::InvalidFrame("invalid LSN routing page".into()));
        }
        // Segment arrays occupy adjacent ranges. Routing is paged too, so segment count cannot grow the resident directory.
        let index = lsn - first;
        let (page, at) = Self::address(first + index, 8)?;
        let offsets = self.page((false, page))?;
        let offset =
            u64::from_le_bytes(offsets.bytes[at..at + 8].try_into().map_err(|_| Error::Poisoned)?);
        Ok(Some((segment, offset)))
    }
}
