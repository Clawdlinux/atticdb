use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use zerodb_tape::{Committed, Digest};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DedupKey(pub(crate) u32, pub(crate) String, pub(crate) String);

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) key: DedupKey,
    pub(crate) digest: Digest,
    pub(crate) committed: Committed,
}

impl Entry {
    pub(crate) fn age(&self) -> u64 {
        match self.committed {
            Committed::Durable { first_lsn, .. } => first_lsn,
            Committed::Fast => unreachable!("engine entries are durable"),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Dedup {
    pub(crate) entries: BTreeMap<u64, Arc<Entry>>,
    lookup: HashMap<DedupKey, u64>,
    tenants: BTreeMap<u32, BTreeSet<u64>>,
    per_tenant: usize,
    global: usize,
}

impl Dedup {
    pub(crate) fn new(per_tenant: usize, global: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            lookup: HashMap::new(),
            tenants: BTreeMap::new(),
            per_tenant,
            global,
        }
    }
    pub(crate) fn get(&self, key: &DedupKey) -> Option<&Arc<Entry>> {
        self.lookup.get(key).and_then(|age| self.entries.get(age))
    }
    pub(crate) fn remove(&mut self, age: u64) -> Option<Arc<Entry>> {
        let entry = self.entries.remove(&age)?;
        self.lookup.remove(&entry.key);
        if let Some(ages) = self.tenants.get_mut(&entry.key.0) {
            ages.remove(&age);
            if ages.is_empty() {
                self.tenants.remove(&entry.key.0);
            }
        }
        Some(entry)
    }
    pub(crate) fn insert(&mut self, entry: Entry) {
        self.insert_arc(Arc::new(entry));
    }
    pub(crate) fn insert_arc(&mut self, entry: Arc<Entry>) {
        if self.per_tenant == 0 || self.global == 0 {
            return;
        }
        if let Some(&age) = self.lookup.get(&entry.key) {
            self.remove(age);
        }
        let tenant = entry.key.0;
        let age = entry.age();
        self.lookup.insert(entry.key.clone(), age);
        self.tenants.entry(tenant).or_default().insert(age);
        self.entries.insert(age, entry);
        while self.tenants.get(&tenant).is_some_and(|ages| ages.len() > self.per_tenant) {
            if let Some(&age) = self.tenants[&tenant].first() {
                self.remove(age);
            }
        }
        while self.entries.len() > self.global {
            if let Some((&age, _)) = self.entries.first_key_value() {
                self.remove(age);
            }
        }
    }
    fn tenant_len(&self, tenant: u32) -> usize {
        self.tenants.get(&tenant).map_or(0, BTreeSet::len)
    }
    pub(crate) fn can_stage(&self, tenant: u32) -> bool {
        self.entries.len() < self.global && self.tenant_len(tenant) < self.per_tenant
    }
    pub(crate) fn effective_window(&self) -> usize {
        self.tenants.values().map(BTreeSet::len).min().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn entry(tenant: u32, session: &str, key: &str, age: u64) -> Entry {
        Entry {
            key: DedupKey(tenant, session.into(), key.into()),
            digest: [3; 32],
            committed: Committed::Durable {
                txn: age,
                first_lsn: age,
                last_lsn: age,
                reply: 1u64.to_le_bytes().to_vec(),
            },
        }
    }
    #[test]
    fn scope_bounds_and_oldest_eviction() {
        let mut dedup = Dedup::new(2, 3);
        dedup.insert(entry(1, "s", "k", 1));
        dedup.insert(entry(1, "other", "k", 2));
        dedup.insert(entry(2, "s", "k", 3));
        assert_eq!(dedup.entries.len(), 3);
        assert_eq!(dedup.effective_window(), 1);
        dedup.insert(entry(1, "s", "new", 4));
        assert!(dedup.get(&DedupKey(1, "s".into(), "k".into())).is_none());
        assert!(dedup.get(&DedupKey(2, "s".into(), "k".into())).is_some());
        dedup.insert(entry(3, "s", "k", 5));
        assert!(!dedup.entries.contains_key(&2));
        assert_eq!(dedup.entries.len(), 3);
    }
}
