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
        self.map.len() + staged.len() >= self.max_keys
            && self.map.len() + staged.keys().filter(|key| !self.map.contains_key(*key)).count()
                >= self.max_keys
    }
    /// Appends a revision. Keeps the newest one below `floor`, the group's first LSN,
    /// so a reader at the previous durable LSN still finds it.
    pub(crate) fn install(&mut self, key: &FactKey, rev: Rev, floor: u64) -> Result<()> {
        self.admit(key)?;
        let revs = self.map.entry(key.clone()).or_default();
        revs.push(rev);
        while revs.len() > self.history + 1 && revs[1].lsn < floor {
            revs.remove(0);
        }
        self.dirty.insert(key.clone());
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
