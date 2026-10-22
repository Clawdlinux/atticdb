use crate::error::Result;
use crate::{EngineError, ErrorKind};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) type FactKey = (u32, String);

/// A revision pointer. The tombstone bit is not content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Rev {
    pub(crate) lsn: u64,
    pub(crate) deleted: bool,
}

/// Revisions a reader at `snapshot` may see, oldest first. Its current value is the newest
/// one, unless that is a tombstone. Never an older revision.
pub(crate) fn visible(revs: &[Rev], snapshot: u64) -> &[Rev] {
    &revs[..revs.partition_point(|rev| rev.lsn <= snapshot)]
}

/// LSN of the current value, or 0 when the key has none.
pub(crate) fn current(newest: Option<&Rev>) -> u64 {
    newest.filter(|rev| !rev.deleted).map_or(0, |rev| rev.lsn)
}

/// Published fact index: per key, revisions oldest first, capped by `max_facts` keys.
#[derive(Debug)]
pub(crate) struct Facts {
    pub(crate) map: BTreeMap<FactKey, Vec<Rev>>,
    pub(crate) dirty: BTreeSet<FactKey>,
    max_keys: usize,
    pub(crate) history: usize,
}
impl Facts {
    pub(crate) fn new(max_keys: usize, history: usize) -> Self {
        Self { map: BTreeMap::new(), dirty: BTreeSet::new(), max_keys, history }
    }
    fn admit(&self, key: &FactKey) -> Result<()> {
        if !self.map.contains_key(key) && self.map.len() >= self.max_keys {
            return Err(EngineError::rejected(ErrorKind::ResourceExhausted("facts")));
        }
        Ok(())
    }
    /// True when a new key cannot be staged next to the group's staged keys.
    pub(crate) fn full(&self, staged: &HashMap<FactKey, Rev>) -> bool {
        self.map.len() + staged.keys().filter(|key| !self.map.contains_key(*key)).count()
            >= self.max_keys
    }
    /// Appends a revision. Keeps the newest `history + 1` revisions, plus the newest one below
    /// `floor`, the publication's first LSN, for readers at the previous durable LSN.
    /// So a key holds at most `history + 2` revisions.
    pub(crate) fn install(&mut self, key: &FactKey, rev: Rev, floor: u64) -> Result<()> {
        let revs = match self.map.get_mut(key) {
            Some(revs) => revs,
            None => {
                self.admit(key)?;
                self.map.entry(key.clone()).or_default()
            }
        };
        revs.push(rev);
        let tail = revs.len().saturating_sub(self.history + 1);
        let below = revs.partition_point(|rev| rev.lsn < floor).saturating_sub(1);
        if below + 1 < tail {
            revs.drain(below + 1..tail);
        }
        revs.drain(..below.min(tail));
        if !self.dirty.contains(key) {
            self.dirty.insert(key.clone());
        }
        Ok(())
    }
    /// Replaces a key's revisions from a checkpoint, trimmed to the current history.
    pub(crate) fn restore(&mut self, key: FactKey, mut revs: Vec<Rev>) -> Result<()> {
        self.admit(&key)?;
        revs.drain(..revs.len().saturating_sub(self.history + 1));
        self.map.insert(key, revs);
        Ok(())
    }
}
