//! The published forget set (S5, D98). Every check is O(1).
//!
//! A session or fact-key forget stores its highest cutoff. An LSN forget stores
//! `(tenant, lsn)` pairs. Entries carry the record header's tenant id (I11), so a pair that
//! names another tenant's record, or no record, never matches and stays inert.

use crate::error::Result;
use crate::facts::FactKey;
use crate::session::Key;
use crate::{EngineError, ErrorKind};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug)]
pub(crate) struct Forgets {
    pub(crate) sessions: HashMap<Key, u64>,
    pub(crate) keys: HashMap<FactKey, u64>,
    pub(crate) lsns: HashSet<(u32, u64)>,
    /// Entries inserted or raised since the last installed checkpoint that held them.
    pub(crate) dirty_sessions: BTreeSet<Key>,
    pub(crate) dirty_keys: BTreeSet<FactKey>,
    pub(crate) dirty_lsns: BTreeSet<(u32, u64)>,
    max_lsns: usize,
}

impl Forgets {
    pub(crate) fn new(max_lsns: usize) -> Self {
        Self {
            sessions: HashMap::new(),
            keys: HashMap::new(),
            lsns: HashSet::new(),
            dirty_sessions: BTreeSet::new(),
            dirty_keys: BTreeSet::new(),
            dirty_lsns: BTreeSet::new(),
            max_lsns,
        }
    }

    /// True when the turn at `lsn` of session `key` is hidden.
    pub(crate) fn hides_turn(&self, key: &Key, lsn: u64) -> bool {
        (!self.sessions.is_empty() && self.sessions.get(key).is_some_and(|&cut| lsn <= cut))
            || self.hides_lsn(key.0, lsn)
    }

    /// True when the revision at `lsn` of fact `key` is hidden.
    pub(crate) fn hides_fact(&self, key: &FactKey, lsn: u64) -> bool {
        (!self.keys.is_empty() && self.keys.get(key).is_some_and(|&cut| lsn <= cut))
            || self.hides_lsn(key.0, lsn)
    }

    pub(crate) fn hides_lsn(&self, tenant: u32, lsn: u64) -> bool {
        !self.lsns.is_empty() && self.lsns.contains(&(tenant, lsn))
    }

    /// The session's cutoff, if any.
    pub(crate) fn session_cutoff(&self, key: &Key) -> Option<u64> {
        if self.sessions.is_empty() {
            return None;
        }
        self.sessions.get(key).copied()
    }

    pub(crate) fn max_lsns(&self) -> usize {
        self.max_lsns
    }

    /// Raises a session's cutoff. Marks it dirty only when it changed.
    pub(crate) fn raise_session(&mut self, key: Key, cutoff: u64) {
        let held = self.sessions.entry(key.clone()).or_insert(0);
        if cutoff > *held {
            *held = cutoff;
            self.dirty_sessions.insert(key);
        }
    }

    /// Raises a fact key's cutoff. Marks it dirty only when it changed.
    pub(crate) fn raise_key(&mut self, key: FactKey, cutoff: u64) {
        let held = self.keys.entry(key.clone()).or_insert(0);
        if cutoff > *held {
            *held = cutoff;
            self.dirty_keys.insert(key);
        }
    }

    /// Adds a pair. Over the cap is `ResourceExhausted("forgets")`, as at prepare.
    pub(crate) fn add_lsn(&mut self, pair: (u32, u64)) -> Result<()> {
        if self.lsns.contains(&pair) {
            return Ok(());
        }
        if self.lsns.len() >= self.max_lsns {
            return Err(EngineError::rejected(ErrorKind::ResourceExhausted("forgets")));
        }
        self.lsns.insert(pair);
        self.dirty_lsns.insert(pair);
        Ok(())
    }
}

impl Forgets {
    /// Restores a checkpointed session cutoff. Not dirty: the chain already holds it.
    pub(crate) fn restore_session(&mut self, key: Key, cutoff: u64) {
        let held = self.sessions.entry(key).or_insert(0);
        *held = (*held).max(cutoff);
    }

    /// Restores a checkpointed fact-key cutoff. Not dirty.
    pub(crate) fn restore_key(&mut self, key: FactKey, cutoff: u64) {
        let held = self.keys.entry(key).or_insert(0);
        *held = (*held).max(cutoff);
    }

    /// Restores a checkpointed pair. Not dirty. A lowered cap fails, as `max_facts` does.
    pub(crate) fn restore_lsn(&mut self, pair: (u32, u64)) -> Result<()> {
        if !self.lsns.contains(&pair) && self.lsns.len() >= self.max_lsns {
            return Err(EngineError::rejected(ErrorKind::ResourceExhausted("forgets")));
        }
        self.lsns.insert(pair);
        Ok(())
    }
}

/// The cutoff a forget at `lsn` applies: `up_to_lsn` when given and lower, else its own LSN.
pub(crate) fn cutoff(up_to_lsn: Option<u64>, lsn: u64) -> u64 {
    up_to_lsn.map_or(lsn, |up_to| up_to.min(lsn))
}

/// Sorted, deduplicated LSNs of a list selector.
pub(crate) fn unique(lsns: &[u64]) -> Vec<u64> {
    let mut lsns = lsns.to_vec();
    lsns.sort_unstable();
    lsns.dedup();
    lsns
}
