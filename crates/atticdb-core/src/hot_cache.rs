//! The hot cache: recent turns, decoded, for `window`.
//!
//! The tape stays the only source of truth. An entry is an immutable copy of a turn that was
//! read from the tape and passed its checks there. A CRC32C over every field is computed on
//! insert and checked on every hit, so a bit flip or a stray write in RAM drops the entry
//! instead of serving it (0007). A miss, an eviction or a dropped entry reads the tape again.
//! It holds at most its byte budget, least recently used out first, and lives per process.

use crate::turn::Role;
use atticdb_tape::Crc32c;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// A turn as `window` returns it, with the identity `window` checks.
#[derive(Debug)]
pub(crate) struct Cached {
    pub(crate) tenant: u32,
    pub(crate) agent: Box<str>,
    pub(crate) session: Box<str>,
    pub(crate) seq: u64,
    pub(crate) commit_time_ns: u64,
    pub(crate) role: Role,
    pub(crate) content: Vec<u8>,
    pub(crate) meta: Vec<u8>,
    crc: u32,
}

impl Cached {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        tenant: u32,
        agent: &str,
        session: &str,
        seq: u64,
        commit_time_ns: u64,
        role: Role,
        content: Vec<u8>,
        meta: Vec<u8>,
    ) -> Self {
        let mut cached = Self {
            tenant,
            agent: agent.into(),
            session: session.into(),
            seq,
            commit_time_ns,
            role,
            content,
            meta,
            crc: 0,
        };
        cached.crc = cached.checksum();
        cached
    }

    /// Every field, each length-prefixed, so moved bytes change the sum too.
    fn checksum(&self) -> u32 {
        let mut crc = Crc32c::new();
        crc.update(&self.tenant.to_le_bytes());
        crc.update(&self.seq.to_le_bytes());
        crc.update(&self.commit_time_ns.to_le_bytes());
        crc.update(&[self.role as u8]);
        for field in [self.agent.as_bytes(), self.session.as_bytes(), &self.meta, &self.content] {
            crc.update(&(field.len() as u64).to_le_bytes());
            crc.update(field);
        }
        crc.finish()
    }

    fn size(&self) -> usize {
        self.content.len() + self.meta.len() + self.agent.len() + self.session.len() + 128
    }
}

#[derive(Debug, Default)]
struct Inner {
    /// LSN to the entry and its last use.
    map: HashMap<u64, (Arc<Cached>, u64)>,
    /// Last use to LSN, oldest first.
    order: BTreeMap<u64, u64>,
    bytes: usize,
    tick: u64,
    hits: u64,
    misses: u64,
    dropped: u64,
}

/// Counters for `/v1/stats`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counts {
    pub(crate) bytes: usize,
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    /// Entries whose checksum failed on a hit.
    pub(crate) dropped: u64,
}

#[derive(Debug)]
pub(crate) struct HotCache {
    inner: Mutex<Inner>,
    budget: usize,
}

impl HotCache {
    /// A zero budget caches nothing.
    pub(crate) fn new(budget: usize) -> Self {
        Self { inner: Mutex::new(Inner::default()), budget }
    }

    /// The entry at `lsn`, checked. `None` on a miss or a failed check, which drops it.
    pub(crate) fn get(&self, lsn: u64) -> Option<Arc<Cached>> {
        if self.budget == 0 {
            return None;
        }
        let entry = {
            let mut inner = self.inner.lock().ok()?;
            let inner = &mut *inner;
            inner.tick += 1;
            let tick = inner.tick;
            let Some((entry, used)) = inner.map.get_mut(&lsn) else {
                inner.misses += 1;
                return None;
            };
            inner.order.remove(used);
            *used = tick;
            inner.order.insert(tick, lsn);
            entry.clone()
        };
        // Checked outside the lock: the entry is immutable and shared.
        if entry.checksum() != entry.crc {
            if let Ok(mut inner) = self.inner.lock() {
                inner.dropped += 1;
                inner.remove(lsn);
            }
            return None;
        }
        if let Ok(mut inner) = self.inner.lock() {
            inner.hits += 1;
        }
        Some(entry)
    }

    /// Caches a turn that passed the tape's checks. Turns over 1/8 of the budget are skipped,
    /// so 1 large turn doesn't empty the cache.
    pub(crate) fn insert(&self, lsn: u64, cached: Cached) {
        let size = cached.size();
        if self.budget == 0 || size > self.budget / 8 {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else { return };
        inner.remove(lsn);
        while inner.bytes + size > self.budget {
            let Some((_, oldest)) = inner.order.pop_first() else { break };
            inner.remove(oldest);
        }
        inner.tick += 1;
        let tick = inner.tick;
        inner.order.insert(tick, lsn);
        inner.map.insert(lsn, (Arc::new(cached), tick));
        inner.bytes += size;
    }

    pub(crate) fn counts(&self) -> Counts {
        self.inner.lock().map_or_else(
            |_| Counts::default(),
            |inner| Counts {
                bytes: inner.bytes,
                hits: inner.hits,
                misses: inner.misses,
                dropped: inner.dropped,
            },
        )
    }
}

impl Inner {
    fn remove(&mut self, lsn: u64) {
        if let Some((entry, used)) = self.map.remove(&lsn) {
            self.order.remove(&used);
            self.bytes -= entry.size();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn turn(seq: u64, size: usize) -> Cached {
        Cached::new(1, "a", "s", seq, seq * 10, Role::User, vec![seq as u8; size], vec![])
    }

    /// Flips 1 bit of a cached entry's content in place, as a RAM fault would.
    fn corrupt(cache: &HotCache, lsn: u64) {
        let mut inner = cache.inner.lock().unwrap();
        let (entry, _) = inner.map.get_mut(&lsn).unwrap();
        let mut copy = Cached {
            content: entry.content.clone(),
            agent: entry.agent.clone(),
            session: entry.session.clone(),
            meta: entry.meta.clone(),
            ..**entry
        };
        copy.content[0] ^= 1;
        *entry = Arc::new(copy);
    }

    #[test]
    fn hits_evict_least_recent_and_drop_corrupt_entries() {
        let cache = HotCache::new(8 * (64 + 1 + 1 + 128));
        for seq in 1..=8 {
            cache.insert(seq, turn(seq, 64));
        }
        assert_eq!(cache.get(1).unwrap().seq, 1, "a hit");
        cache.insert(9, turn(9, 64));
        assert!(cache.get(2).is_none(), "2 was least recently used, so it went");
        assert!(cache.get(1).is_some(), "1 was used, so it stayed");
        cache.insert(10, turn(10, 10_000));
        assert!(cache.get(10).is_none(), "over 1/8 of the budget: not cached");
        corrupt(&cache, 3);
        assert!(cache.get(3).is_none(), "a flipped bit is never served");
        assert!(cache.get(3).is_none(), "and the entry is gone");
        let counts = cache.counts();
        assert_eq!((counts.hits, counts.dropped), (2, 1));
        assert_eq!(counts.bytes, 7 * (64 + 1 + 1 + 128), "8 fit, 1 evicted, 1 dropped, 1 too big");
        assert!(HotCache::new(0).get(1).is_none());
    }
}
