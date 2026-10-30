use atticdb_tape::{Committed, Digest};
use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};
use std::sync::{Arc, OnceLock};

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
    /// `hash_key(&key)`, kept so staging, publication and eviction never rehash.
    pub(crate) hash: u64,
    pub(crate) key: DedupKey,
    pub(crate) digest: Digest,
    pub(crate) committed: Committed,
}

impl Entry {
    pub(crate) fn new(key: DedupKey, digest: Digest, committed: Committed) -> Self {
        Self { hash: hash_key(&key), key, digest, committed }
    }

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
    /// Key hash to entry age. Sharded so a growing map rehashes 1/64 of its keys at a
    /// time, under the state lock. Equality is checked on the stored entry's full key.
    lookup: Vec<HashMap<u64, Ages, BuildHasherDefault<Prehashed>>>,
    tenants: BTreeMap<u32, BTreeSet<u64>>,
    per_tenant: usize,
    global: usize,
}

/// Ages sharing one 64-bit key hash. More than one only on a real collision.
#[derive(Debug)]
enum Ages {
    One(u64),
    Many(Vec<u64>),
}

impl Ages {
    fn as_slice(&self) -> &[u64] {
        match self {
            Ages::One(age) => std::slice::from_ref(age),
            Ages::Many(ages) => ages,
        }
    }
}

const SHARDS: usize = 64;

/// Passes a precomputed u64 through. Only `lookup` uses it, keyed by `hash_key`.
#[derive(Default)]
struct Prehashed(u64);

impl Hasher for Prehashed {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("lookup keys are u64")
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// One keyed SipHash per key. Random keys per process, so a tenant cannot pick
/// idempotency keys that collide. Shared by every `Dedup`, so one hash serves both
/// the committed and the staged window.
pub(crate) fn hash_key(key: &DedupKey) -> u64 {
    static STATE: OnceLock<RandomState> = OnceLock::new();
    STATE.get_or_init(RandomState::new).hash_one(key)
}

/// Bits 32 to 37. `HashMap` takes its 7-bit tag from the top bits and its bucket from the
/// low bits, and `Prehashed` passes the hash through, so the shard must use neither.
fn shard(hash: u64) -> usize {
    (hash >> 32) as usize % SHARDS
}

impl Dedup {
    pub(crate) fn new(per_tenant: usize, global: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            lookup: (0..SHARDS).map(|_| HashMap::default()).collect(),
            tenants: BTreeMap::new(),
            per_tenant,
            global,
        }
    }
    pub(crate) fn get(&self, key: &DedupKey) -> Option<&Arc<Entry>> {
        self.get_hashed(hash_key(key), key)
    }
    pub(crate) fn get_hashed(&self, hash: u64, key: &DedupKey) -> Option<&Arc<Entry>> {
        self.lookup[shard(hash)]
            .get(&hash)?
            .as_slice()
            .iter()
            .filter_map(|age| self.entries.get(age))
            .find(|entry| entry.key == *key)
    }
    pub(crate) fn remove(&mut self, age: u64) -> Option<Arc<Entry>> {
        let entry = self.entries.remove(&age)?;
        self.unlink(entry.hash, age);
        if let Some(ages) = self.tenants.get_mut(&entry.key.0) {
            ages.remove(&age);
            if ages.is_empty() {
                self.tenants.remove(&entry.key.0);
            }
        }
        Some(entry)
    }
    fn unlink(&mut self, hash: u64, age: u64) {
        let map = &mut self.lookup[shard(hash)];
        match map.get_mut(&hash) {
            Some(Ages::One(a)) if *a == age => {
                map.remove(&hash);
            }
            Some(Ages::Many(ages)) => {
                ages.retain(|&a| a != age);
                if let [last] = ages[..] {
                    map.insert(hash, Ages::One(last));
                }
            }
            _ => {}
        }
    }
    fn link(&mut self, hash: u64, age: u64) {
        let map = &mut self.lookup[shard(hash)];
        match map.remove(&hash) {
            None => map.insert(hash, Ages::One(age)),
            Some(Ages::One(a)) => map.insert(hash, Ages::Many(vec![a, age])),
            Some(Ages::Many(mut ages)) => {
                ages.push(age);
                map.insert(hash, Ages::Many(ages))
            }
        };
    }
    pub(crate) fn insert(&mut self, entry: Entry) {
        self.insert_arc(Arc::new(entry));
    }
    pub(crate) fn insert_arc(&mut self, entry: Arc<Entry>) {
        if self.per_tenant == 0 || self.global == 0 {
            return;
        }
        let hash = entry.hash;
        if let Some(old) = self.get_hashed(hash, &entry.key).map(|e| e.age()) {
            self.remove(old);
        }
        let tenant = entry.key.0;
        let age = entry.age();
        self.link(hash, age);
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
    use super::*;

    fn key(tenant: u32, name: &str) -> DedupKey {
        DedupKey(tenant, Scope::Fact(name.into()), "op".into())
    }

    fn entry(key: DedupKey, age: u64) -> Entry {
        let committed =
            Committed::Durable { txn: age, first_lsn: age, last_lsn: age, reply: vec![] };
        Entry::new(key, digest([b"x".as_slice()]), committed)
    }

    #[test]
    fn hash_is_stable_within_process() {
        assert_eq!(hash_key(&key(1, "a")), hash_key(&key(1, "a")));
        assert_ne!(hash_key(&key(1, "a")), hash_key(&key(2, "a")));
    }

    #[test]
    fn insert_get_replace_remove() {
        let mut d = Dedup::new(10, 10);
        d.insert(entry(key(1, "a"), 1));
        d.insert(entry(key(1, "b"), 2));
        assert_eq!(d.get(&key(1, "a")).map(|e| e.age()), Some(1));
        d.insert(entry(key(1, "a"), 3));
        assert_eq!(d.get(&key(1, "a")).map(|e| e.age()), Some(3));
        assert_eq!(d.entries.len(), 2);
        d.remove(3);
        assert!(d.get(&key(1, "a")).is_none());
        assert_eq!(d.get(&key(1, "b")).map(|e| e.age()), Some(2));
        assert!(d.lookup.iter().map(HashMap::len).sum::<usize>() == 1);
    }

    #[test]
    fn eviction_unlinks_lookup() {
        let mut d = Dedup::new(2, 3);
        for (age, name) in ["a", "b", "c"].into_iter().enumerate() {
            d.insert(entry(key(1, name), age as u64));
        }
        assert!(d.get(&key(1, "a")).is_none());
        d.insert(entry(key(2, "x"), 10));
        d.insert(entry(key(3, "y"), 11));
        assert_eq!(d.entries.len(), 3);
        assert!(d.get(&key(1, "b")).is_none());
        assert_eq!(d.lookup.iter().map(HashMap::len).sum::<usize>(), 3);
    }

    #[test]
    fn colliding_hashes_compare_full_key() {
        let mut d = Dedup::new(10, 10);
        let (a, b) = (key(1, "a"), key(1, "b"));
        d.entries.insert(1, Arc::new(entry(a.clone(), 1)));
        d.entries.insert(2, Arc::new(entry(b.clone(), 2)));
        d.link(7, 1);
        d.link(7, 2);
        assert_eq!(d.get_hashed(7, &a).map(|e| e.age()), Some(1));
        assert_eq!(d.get_hashed(7, &b).map(|e| e.age()), Some(2));
        assert!(d.get_hashed(7, &key(1, "c")).is_none());
        d.unlink(7, 1);
        assert!(d.get_hashed(7, &a).is_none());
        assert_eq!(d.get_hashed(7, &b).map(|e| e.age()), Some(2));
        d.unlink(7, 2);
        assert!(d.lookup[shard(7)].is_empty());
    }

    #[test]
    fn hash_serves_both_instances() {
        let mut committed = Dedup::new(10, 10);
        let staged = Dedup::new(10, 10);
        committed.insert(entry(key(1, "a"), 1));
        let h = hash_key(&key(1, "a"));
        assert!(staged.get_hashed(h, &key(1, "a")).is_none());
        assert_eq!(committed.get_hashed(h, &key(1, "a")).map(|e| e.age()), Some(1));
    }
}
