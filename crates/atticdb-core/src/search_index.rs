//! The word index behind `Engine::search`.
//!
//! Search keeps a record when every query word, or its singular, is a substring of the
//! record's lowercased text. A query word holds no whitespace, so any match lies inside 1
//! whitespace-delimited token of that text. So each tenant keeps a dictionary of the lowercase
//! tokens of its turns and facts, and per token the LSNs that hold it. A query scans the
//! dictionary, not the records.
//!
//! The index only proposes candidates. Search reads each one through the normal read path and
//! checks it again, so a stale, partial or stopped index can make a search slower, never
//! wrong. It's built off the writer from durable records in LSN order, from plain text, so it
//! never inflates at query time. It lives in memory. After a restart it rebuilds in the
//! background, and search scans the records above its watermark.

use atticdb_tape::{DurableReads, Kind};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The text search matches: lossy UTF-8, lowercased as a whole, as `found` in `search` does.
pub(crate) fn lower(field: &[u8]) -> String {
    String::from_utf8_lossy(field).to_lowercase()
}

/// Records indexed per lock hold.
const BATCH: usize = 256;
/// How long the builder sleeps when it has caught up.
const IDLE: Duration = Duration::from_millis(20);

#[derive(Debug, Default)]
pub(crate) struct Index {
    tenants: HashMap<u32, Terms>,
    /// Every record at or below this LSN is indexed.
    pub(crate) lsn: u64,
    /// Approximate heap bytes held.
    pub(crate) bytes: usize,
    pub(crate) terms: usize,
    budget: usize,
    /// Why indexing stopped. Search then scans everything above `lsn`.
    pub(crate) stopped: Option<String>,
}

#[derive(Debug, Default)]
struct Terms {
    /// Token hash to id. The token's text lives once, in `blob`, and is compared on lookup.
    ids: HashMap<u64, u32>,
    /// Tokens whose hash an earlier token already holds.
    collided: HashMap<Box<str>, u32>,
    hasher: std::collections::hash_map::RandomState,
    /// Every token, each followed by `\n`, in id order. A query word has no whitespace, so a
    /// match never crosses a separator.
    blob: String,
    /// Where each token starts in `blob`.
    starts: Vec<u32>,
    postings: Vec<Postings>,
    /// Per 3-byte window, the ids of the tokens holding it.
    grams: HashMap<u32, Postings>,
    /// Gives every token the same hash, so tests reach `collided`.
    #[cfg(test)]
    collide: bool,
}

/// Ascending values, such as LSNs or token ids, as LEB128 deltas.
#[derive(Debug, Default)]
struct Postings {
    deltas: Vec<u8>,
    last: u64,
    count: u32,
}

impl Postings {
    /// Appends `value`, at least the last one. Returns the bytes added.
    fn push(&mut self, value: u64) -> usize {
        let (held, mut delta) = (self.deltas.len(), value - self.last);
        loop {
            let byte = (delta & 0x7f) as u8;
            delta >>= 7;
            if delta == 0 {
                self.deltas.push(byte);
                break;
            }
            self.deltas.push(byte | 0x80);
        }
        (self.last, self.count) = (value, self.count + 1);
        self.deltas.len() - held
    }

    /// The values, ascending.
    fn values(&self) -> impl Iterator<Item = u64> + '_ {
        let (mut value, mut shift, mut delta) = (0u64, 0, 0u64);
        self.deltas.iter().filter_map(move |&byte| {
            delta |= u64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 != 0 {
                return None;
            }
            value += delta;
            (shift, delta) = (0, 0);
            Some(value)
        })
    }
}

/// A token's 3-byte window, as a key.
fn gram(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2])
}

/// Heap cost charged per new token beyond its text: the hash map entry, `starts` and
/// `Postings`.
const TERM_OVERHEAD: usize = 56;
/// Heap cost charged per new trigram: its map entry and `Postings`.
const GRAM_OVERHEAD: usize = 64;

impl Index {
    fn new(budget: usize) -> Self {
        Self { budget, ..Self::default() }
    }

    /// Adds 1 record. Returns false once the budget stops indexing; `lsn` then stays put.
    fn add(&mut self, tenant: u32, lsn: u64, fields: &[Vec<u8>]) -> bool {
        if self.stopped.is_some() {
            return false;
        }
        if self.bytes >= self.budget {
            self.stopped = Some(format!("memory budget of {} bytes reached", self.budget));
            return false;
        }
        let lowered: Vec<String> = fields.iter().map(|field| lower(field)).collect();
        let mut seen = HashSet::new();
        let terms = self.tenants.entry(tenant).or_default();
        for token in lowered.iter().flat_map(|text| text.split_whitespace()) {
            if !seen.insert(token) {
                continue;
            }
            let id = match terms.id(token) {
                Some(id) => id,
                None => {
                    let (Ok(id), Ok(start)) =
                        (u32::try_from(terms.postings.len()), u32::try_from(terms.blob.len()))
                    else {
                        self.stopped = Some("dictionary past 4 GiB".into());
                        return false;
                    };
                    let hash = terms.hash(token);
                    match terms.ids.entry(hash) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            terms.collided.insert(token.into(), id);
                            self.bytes += token.len() + TERM_OVERHEAD;
                        }
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert(id);
                        }
                    }
                    terms.starts.push(start);
                    terms.blob.push_str(token);
                    terms.blob.push('\n');
                    terms.postings.push(Postings::default());
                    let mut keys: Vec<u32> = token.as_bytes().windows(3).map(gram).collect();
                    keys.sort_unstable();
                    keys.dedup();
                    for key in keys {
                        let list = terms.grams.entry(key).or_default();
                        if list.count == 0 {
                            self.bytes += GRAM_OVERHEAD;
                        }
                        self.bytes += list.push(u64::from(id));
                    }
                    self.bytes += token.len() + 1 + TERM_OVERHEAD;
                    self.terms += 1;
                    id
                }
            };
            let postings = &mut terms.postings[id as usize];
            self.bytes += postings.push(lsn);
        }
        self.lsn = lsn;
        true
    }

    /// LSNs below `below` holding every word of `words`, or its singular, newest first. A
    /// superset only: unselective words are skipped, and search checks every candidate.
    pub(crate) fn candidates(
        &self,
        tenant: u32,
        words: &[(String, Option<String>)],
        below: u64,
    ) -> Vec<u64> {
        let Some(terms) = self.tenants.get(&tenant) else { return Vec::new() };
        let mut sets: Vec<(u64, Vec<u32>)> = words
            .iter()
            .map(|(word, stem)| {
                let mut ids = terms.matching(word);
                if let Some(stem) = stem {
                    ids.extend(terms.matching(stem));
                    ids.sort_unstable();
                    ids.dedup();
                }
                let total = ids.iter().map(|&id| u64::from(terms.postings[id as usize].count));
                (total.sum(), ids)
            })
            .collect();
        sets.sort_by_key(|(total, _)| *total);
        let Some(((_, first), rest)) = sets.split_first() else { return Vec::new() };
        let mut found = terms.union(first, below);
        for (total, ids) in rest {
            // Decoding a common word costs more than checking the few candidates left.
            if found.is_empty() || *total > 8 * found.len() as u64 {
                break;
            }
            let other = terms.union(ids, below);
            found.retain(|lsn| other.binary_search(lsn).is_ok());
        }
        found.reverse();
        found
    }
}

impl Terms {
    fn hash(&self, token: &str) -> u64 {
        use std::hash::BuildHasher;
        #[cfg(test)]
        if self.collide {
            return 0;
        }
        self.hasher.hash_one(token)
    }

    /// The text of token `id`.
    fn text(&self, id: u32) -> &str {
        let start = self.starts[id as usize] as usize;
        let end = self.starts.get(id as usize + 1).map_or(self.blob.len(), |&end| end as usize);
        &self.blob[start..end - 1]
    }

    fn id(&self, token: &str) -> Option<u32> {
        match self.ids.get(&self.hash(token)) {
            Some(&id) if self.text(id) == token => Some(id),
            Some(_) => self.collided.get(token).copied(),
            None => None,
        }
    }

    /// Ids of the tokens containing `word`, ascending. A word of 3 bytes or more starts from
    /// its 2 rarest trigrams: a token holding the word holds all of them. Shorter words scan
    /// the dictionary. Every candidate is checked with `contains`, so both give the same ids.
    fn matching(&self, word: &str) -> Vec<u32> {
        let bytes = word.as_bytes();
        if bytes.len() < 3 {
            let mut ids: Vec<u32> = self
                .blob
                .match_indices(word)
                .map(|(at, _)| (self.starts.partition_point(|&s| s as usize <= at) - 1) as u32)
                .collect();
            ids.dedup();
            return ids;
        }
        let mut keys: Vec<u32> = bytes.windows(3).map(gram).collect();
        keys.sort_unstable();
        keys.dedup();
        let Some(mut lists) =
            keys.iter().map(|key| self.grams.get(key)).collect::<Option<Vec<_>>>()
        else {
            return Vec::new();
        };
        lists.sort_by_key(|list| list.count);
        let mut ids: Vec<u64> = lists[0].values().collect();
        if let Some(second) = lists.get(1) {
            let mut other = second.values().peekable();
            ids.retain(|id| {
                while other.next_if(|next| next < id).is_some() {}
                other.peek() == Some(id)
            });
        }
        ids.into_iter().map(|id| id as u32).filter(|&id| self.text(id).contains(word)).collect()
    }

    /// Ascending, deduplicated LSNs below `below` of the tokens `ids`.
    fn union(&self, ids: &[u32], below: u64) -> Vec<u64> {
        let mut lsns = Vec::new();
        for &id in ids {
            lsns.extend(self.postings[id as usize].values().take_while(|&lsn| lsn < below));
        }
        lsns.sort_unstable();
        lsns.dedup();
        lsns
    }
}

/// The text of a durable record that search matches: turn content, or fact key and content.
fn fields(kind: Kind, payload: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let bad = |what: &str| format!("undecodable {what} payload");
    Ok(match kind {
        Kind::Turn => vec![crate::turn::decode(payload).map_err(|_| bad("turn"))?.turn.content],
        Kind::Fact => {
            let fact = crate::fact::decode(payload).map_err(|_| bad("fact"))?;
            vec![fact.key.into_bytes(), fact.content]
        }
        _ => Vec::new(),
    })
}

/// Indexes every durable record above the watermark, in batches.
fn catch_up(reads: &DurableReads, index: &Mutex<Index>) -> Result<(), String> {
    let lock = || index.lock().map_err(|_| "index lock poisoned".to_owned());
    let (from, end) = (lock()?.lsn + 1, reads.durable_lsn());
    let mut reader = reads.reader();
    let mut batch = Vec::with_capacity(BATCH);
    for lsn in from..=end {
        let frame = reader.read(lsn).map_err(|e| e.to_string())?;
        let frame = frame.ok_or_else(|| format!("durable LSN {lsn} is unreadable"))?;
        let header = &frame.header;
        batch.push((header.tenant_id, lsn, fields(header.kind, &frame.payload)?));
        if batch.len() == BATCH || lsn == end {
            let mut index = lock()?;
            for (tenant, lsn, fields) in batch.drain(..) {
                if !index.add(tenant, lsn, &fields) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Owns the index and the thread that builds it.
#[derive(Debug)]
pub(crate) struct Indexer {
    index: Arc<Mutex<Index>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Simulations catch up inline, before each search, so runs stay deterministic.
    inline: Option<DurableReads>,
}

impl Indexer {
    /// A zero `budget` turns the index off: search scans, as before it existed.
    pub(crate) fn start(reads: DurableReads, budget: usize, inline: bool) -> std::io::Result<Self> {
        let index = Arc::new(Mutex::new(Index::new(budget)));
        let stop = Arc::new(AtomicBool::new(false));
        let mut indexer =
            Self { index: index.clone(), stop: stop.clone(), thread: None, inline: None };
        if budget == 0 {
            if let Ok(mut index) = index.lock() {
                index.stopped = Some("off".into());
            }
            return Ok(indexer);
        }
        if inline {
            indexer.inline = Some(reads);
            return Ok(indexer);
        }
        let build = move || {
            while !stop.load(Ordering::Acquire) {
                if let Err(error) = catch_up(&reads, &index) {
                    if let Ok(mut index) = index.lock() {
                        index.stopped = Some(error);
                    }
                    return;
                }
                if index.lock().map_or(true, |index| index.stopped.is_some()) {
                    return;
                }
                std::thread::park_timeout(IDLE);
            }
        };
        indexer.thread =
            Some(std::thread::Builder::new().name("atticdb-index".into()).spawn(build)?);
        Ok(indexer)
    }

    /// Runs `f` on the index as it is.
    pub(crate) fn with<T>(&self, f: impl FnOnce(&Index) -> T) -> Option<T> {
        self.index.lock().ok().map(|index| f(&index))
    }

    /// Like [`Indexer::with`]. Inline, it first catches up, so search sees every durable
    /// record through the index.
    pub(crate) fn current<T>(&self, f: impl FnOnce(&Index) -> T) -> Option<T> {
        if let Some(reads) = &self.inline {
            if let Err(error) = catch_up(reads, &self.index) {
                self.index.lock().ok()?.stopped = Some(error);
            }
        }
        self.with(f)
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn colliding_hashes_and_wide_lsn_gaps_keep_postings_exact() {
        let mut index = Index::new(usize::MAX);
        index.tenants.entry(1).or_default().collide = true;
        let records: [(u64, &[u8]); 5] = [
            (1, b"alpha beta"),
            (200, b"beta gamma"),
            (70_000, b"ALPHA"),
            (1 << 40, b"gamma alpha alpha"),
            ((1 << 40) + 1, b"\xffbeta"),
        ];
        for (lsn, text) in records {
            assert!(index.add(1, lsn, &[text.to_vec()]));
        }
        let find = |word: &str, below| index.candidates(1, &[(word.into(), None)], below);
        assert_eq!(find("alpha", u64::MAX), [1 << 40, 70_000, 1]);
        // 3 bytes or more go through trigrams. Shorter words scan the dictionary.
        assert_eq!(find("lph", u64::MAX), [1 << 40, 70_000, 1]);
        assert_eq!(find("ph", u64::MAX), [1 << 40, 70_000, 1]);
        assert_eq!(find("amma", u64::MAX), [1 << 40, 200]);
        assert_eq!(find("\u{fffd}be", u64::MAX), [(1 << 40) + 1]);
        assert_eq!(find("alphab", u64::MAX), Vec::<u64>::new(), "every trigram, but no token");
        assert_eq!(find("beta", u64::MAX), [(1 << 40) + 1, 200, 1]);
        assert_eq!(find("gamma", 1 << 40), [200], "below is exclusive");
        assert_eq!(find("delta", u64::MAX), Vec::<u64>::new());
        assert_eq!(index.candidates(2, &[("alpha".into(), None)], u64::MAX), Vec::<u64>::new());
        let terms = &index.tenants[&1];
        // alpha, beta, gamma and "\u{fffd}beta": all hash to 0, so 3 go to `collided`.
        assert_eq!((terms.postings.len(), terms.collided.len()), (4, 3));
        assert_eq!((index.lsn, index.terms), ((1 << 40) + 1, 4));
    }
}
