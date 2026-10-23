use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use zerodb_tape::{Committed, Digest};

use crate::error::Result;
use crate::{EngineError, ErrorKind};

/// Retry scope inside a tenant. A session and a fact key with one name never collide.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Scope {
    Session(String),
    Fact(String),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DedupKey(pub(crate) u32, pub(crate) Scope, pub(crate) String);

/// BLAKE3 over each payload's length and bytes: a request's retry identity.
pub(crate) fn digest<'a>(payloads: impl IntoIterator<Item = &'a [u8]>) -> Digest {
    let mut hash = blake3::Hasher::new();
    for payload in payloads {
        hash.update(&(payload.len() as u32).to_le_bytes());
        hash.update(payload);
    }
    *hash.finalize().as_bytes()
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) key: DedupKey,
    pub(crate) digest: Digest,
    pub(crate) committed: Committed,
}

impl Entry {
    pub(crate) fn resolve(&self, digest: Digest) -> Result<Committed> {
        if self.digest == digest {
            Ok(self.committed.clone())
        } else {
            Err(EngineError::rejected(ErrorKind::IdempotencyConflict))
        }
    }

    pub(crate) fn age(&self) -> u64 {
        match self.committed {
            Committed::Durable { first_lsn, .. } => first_lsn,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Dedup {
    pub(crate) entries: BTreeMap<u64, Arc<Entry>>,
    /// Sharded so a growing map rehashes 1/64 of its keys at a time, under the state lock.
    lookup: Vec<HashMap<DedupKey, u64>>,
    tenants: BTreeMap<u32, BTreeSet<u64>>,
    per_tenant: usize,
    global: usize,
}

const SHARDS: usize = 64;

fn shard(key: &DedupKey) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish() as usize % SHARDS
}

impl Dedup {
    pub(crate) fn new(per_tenant: usize, global: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            lookup: (0..SHARDS).map(|_| HashMap::new()).collect(),
            tenants: BTreeMap::new(),
            per_tenant,
            global,
        }
    }
    pub(crate) fn get(&self, key: &DedupKey) -> Option<&Arc<Entry>> {
        self.lookup[shard(key)].get(key).and_then(|age| self.entries.get(age))
    }
    pub(crate) fn remove(&mut self, age: u64) -> Option<Arc<Entry>> {
        let entry = self.entries.remove(&age)?;
        self.lookup[shard(&entry.key)].remove(&entry.key);
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
        if let Some(&age) = self.lookup[shard(&entry.key)].get(&entry.key) {
            self.remove(age);
        }
        let tenant = entry.key.0;
        let age = entry.age();
        self.lookup[shard(&entry.key)].insert(entry.key.clone(), age);
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
