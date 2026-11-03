#![cfg(feature = "sim")]
#![allow(clippy::unwrap_used)]

use atticdb_core::{
    forget::Selector, turn::decode, Appended, CompressionMode, Engine, EngineError, EngineOptions,
    ErrorKind, FactQuery, FactWritten, Forgotten, Health, Hit, NewFact, NewTurn, Outcome, Record,
    RecordQuery, Revision, Role, Scope, SearchQuery, SessionQuery, SimFact, Turn, WindowQuery,
};
use atticdb_tape::sim::{CapacityError, CapacityPlan, PublishPause, SimClock, StorageFullFault};
use atticdb_tape::{Clock, Error, HookIo, Io, Kind, Op, SimDisk, Tape, TapeOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut n = self.0;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        n ^ (n >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
    /// Random bytes half the time, else 2-symbol text that deflate shrinks, so both turn
    /// payload versions reach the tape.
    fn content(&mut self, n: usize) -> Vec<u8> {
        if self.below(2) == 0 {
            return self.bytes(n);
        }
        (0..n).map(|_| b"ab"[self.below(2)]).collect()
    }
}

/// Runs `f` once, right after the `n`th later mutating call. Store 0 in the result to disarm.
/// A cut taken here precedes the caller's reply, so later writes cannot alter the image.
fn after_nth(io: &HookIo, n: usize, f: impl FnOnce() + Send + 'static) -> Arc<AtomicUsize> {
    let (left, f) = (Arc::new(AtomicUsize::new(n)), Mutex::new(Some(f)));
    let count = left.clone();
    io.add(move |op, next| {
        let result = next();
        if op.mutates()
            && count.load(Ordering::SeqCst) > 0
            && count.fetch_sub(1, Ordering::SeqCst) == 1
        {
            let f = f.lock().unwrap().take();
            f.into_iter().for_each(|f| f());
        }
        result
    });
    left
}

/// Arms a crash image after the `n`th later mutating call. Zero never fires.
/// The returned call disarms the cut and yields the image, if one was taken.
fn cut_at(io: &HookIo, n: usize) -> impl FnOnce() -> Option<SimDisk> {
    let (disk, image) = (io.inner.clone(), Arc::new(Mutex::new(None)));
    let out = image.clone();
    let left = after_nth(io, n, move || *out.lock().unwrap() = Some(disk.crash().unwrap()));
    move || {
        left.store(0, Ordering::SeqCst);
        image.lock().unwrap().take()
    }
}

/// Fails the next sync once: before it reaches the disk, or `after` it persisted.
/// Store 0 in the result to disarm.
fn fail_sync(io: &HookIo, after: bool) -> Arc<AtomicUsize> {
    let (armed, disk) = (Arc::new(AtomicUsize::new(1)), io.inner.clone());
    let left = armed.clone();
    io.add(move |op, next| {
        if !op.is_sync() {
            return next();
        }
        if !after && left.swap(0, Ordering::SeqCst) == 1 {
            disk.fail_next_sync()?;
        }
        let result = next();
        if after && left.swap(0, Ordering::SeqCst) == 1 && result.is_ok() {
            return Err(Error::SyncFailed("injected error after persistence".into()));
        }
        result
    });
    armed
}

/// Arms the plan's `StorageFull` on the next data-segment write or sync.
fn storage_full(io: &HookIo, plan: &CapacityPlan) {
    let (fault, disk) = (StorageFullFault::default(), io.inner.clone());
    fault.arm(plan);
    io.add(move |op, next| {
        fault.before(&disk, &op);
        next()
    });
}

#[derive(Clone)]
struct Batch {
    scope: usize,
    turns: Vec<NewTurn>,
    key: Option<String>,
    receipt: Option<Appended>,
    time: u64,
    fact: Option<FactOp>,
    forget: Option<ForgetOp>,
}
/// A forget in the scope's tenant. It covers records at or below `up_to_lsn` and its own LSN.
#[derive(Clone)]
struct ForgetOp {
    selector: Selector,
    up_to_lsn: Option<u64>,
}
/// A fact write in the scope's tenant. `content` None deletes. The agent names the batch.
#[derive(Clone)]
struct FactOp {
    key: String,
    content: Option<Vec<u8>>,
    if_lsn: Option<u64>,
}
/// Eight keys per tenant with shared prefixes.
const FACT_KEYS: [&str; 8] = ["a", "a/1", "a/2", "a/b/1", "a/b/2", "ab", "b/1", "b/2"];
type FactKey = (String, String);
/// Per (tenant, key), every acknowledged revision in LSN order. None is a tombstone.
type Revisions = Vec<(u64, Option<Vec<u8>>)>;
/// Per tenant, every acknowledged forget in LSN order as (LSN, selector, cutoff).
type Forgets = BTreeMap<String, Vec<(u64, Selector, u64)>>;
/// A record a forget can cover: a turn of a scope, or a fact revision of a key.
enum Target<'a> {
    Turn(&'a Scope),
    Fact(&'a FactKey),
}
#[derive(Clone)]
struct Oracle {
    scopes: Vec<Scope>,
    history: Vec<Vec<Turn>>,
    batches: Vec<Batch>,
    tenants: BTreeMap<String, u32>,
    facts: BTreeMap<FactKey, Revisions>,
    forgets: Forgets,
    fact_history: usize,
    /// Commit time of the newest fact or forget. They advance the tape's commit clock too.
    fact_time: u64,
}
impl Oracle {
    fn new() -> Self {
        let scopes = (0..48)
            .map(|i| Scope {
                tenant: format!("tenant-{}", i % 4),
                agent: format!("agent-{}", i / 24),
                session: format!("session-{}", i / 4 % 6),
            })
            .collect::<Vec<_>>();
        Self {
            history: vec![Vec::new(); scopes.len()],
            scopes,
            batches: Vec::new(),
            tenants: BTreeMap::new(),
            facts: BTreeMap::new(),
            forgets: BTreeMap::new(),
            fact_history: EngineOptions::default().fact_history,
            fact_time: 0,
        }
    }
    /// I10: a record is hidden exactly when an acknowledged forget of its own tenant covers it.
    fn hidden(&self, target: Target, lsn: u64) -> bool {
        let tenant = match target {
            Target::Turn(scope) => &scope.tenant,
            Target::Fact((tenant, _)) => tenant,
        };
        let forgets = self.forgets.get(tenant).map_or(&[][..], Vec::as_slice);
        forgets.iter().any(|(_, selector, cutoff)| {
            lsn <= *cutoff
                && match (selector, &target) {
                    (Selector::Session { agent, session }, Target::Turn(scope)) => {
                        *agent == scope.agent && *session == scope.session
                    }
                    (Selector::FactKey(key), Target::Fact((_, fact_key))) => key == fact_key,
                    (Selector::Lsns(lsns), _) => lsns.contains(&lsn),
                    _ => false,
                }
        })
    }
    /// The scope's turns as readers must return them: a forgotten turn is a placeholder.
    fn window(&self, scope: usize) -> Vec<Turn> {
        let hide = |turn: &Turn| self.hidden(Target::Turn(&self.scopes[scope]), turn.lsn);
        let placeholder = |turn: &Turn| Turn {
            lsn: turn.lsn,
            seq: turn.seq,
            commit_time_ns: 0,
            role: Role::System,
            content: vec![],
            meta: vec![],
            redacted: true,
        };
        let turns = self.history[scope].iter();
        turns.map(|turn| if hide(turn) { placeholder(turn) } else { turn.clone() }).collect()
    }
    /// A full window equals the oracle's and costs 1 read per turn that is not a placeholder.
    /// Returns the expected window.
    fn check_window(&self, engine: &Engine, scope: usize, case: &str) -> Vec<Turn> {
        let expected = self.window(scope);
        let before = engine.metrics().unwrap().window_reads;
        let got = engine.window(&self.scopes[scope], WindowQuery::Last(usize::MAX)).unwrap();
        assert_eq!(got, expected, "I10: {case} scope={scope}");
        let reads = expected.iter().filter(|turn| !turn.redacted).count() as u64;
        let read = engine.metrics().unwrap().window_reads - before;
        assert_eq!(read, reads, "window reads: {case} scope={scope}");
        expected
    }
    /// A revision as readers must return it: a forgotten one is a placeholder.
    fn revision(&self, key: &FactKey, lsn: u64, content: &Option<Vec<u8>>) -> Revision {
        if self.hidden(Target::Fact(key), lsn) {
            let (agent, content, meta) = (String::new(), vec![], vec![]);
            return Revision {
                lsn,
                commit_time_ns: 0,
                agent,
                deleted: false,
                content,
                meta,
                redacted: true,
            };
        }
        let index = self.fact_batch(lsn);
        Revision {
            lsn,
            commit_time_ns: self.batches[index].time,
            agent: index.to_string(),
            deleted: content.is_none(),
            content: content.clone().unwrap_or_default(),
            meta: vec![],
            redacted: false,
        }
    }
    /// The acknowledged fact batch that wrote the revision at `lsn`.
    fn fact_batch(&self, lsn: u64) -> usize {
        self.batches
            .iter()
            .position(|b| b.fact.is_some() && b.receipt.as_ref().is_some_and(|r| r.last_lsn == lsn))
            .unwrap()
    }
    /// The key's current value: the newest revision unless it is a tombstone or forgotten (I9).
    fn current_value(&self, key: &FactKey) -> Option<(u64, Vec<u8>)> {
        let (lsn, content) = self.facts.get(key)?.last()?;
        if self.hidden(Target::Fact(key), *lsn) {
            return None;
        }
        Some((*lsn, content.clone()?))
    }
    /// The scopes and fact keys a forget batch names, in every tenant for LSNs.
    fn touched(&self, index: usize) -> (Vec<usize>, Vec<FactKey>) {
        let tenant = &self.scopes[self.batches[index].scope].tenant;
        match &self.batches[index].forget.as_ref().unwrap().selector {
            Selector::Session { agent, session } => {
                let scopes = (0..self.scopes.len()).filter(|&s| {
                    let scope = &self.scopes[s];
                    scope.tenant == *tenant && scope.agent == *agent && scope.session == *session
                });
                (scopes.collect(), vec![])
            }
            Selector::FactKey(key) => (vec![], vec![(tenant.clone(), key.clone())]),
            Selector::Lsns(lsns) => {
                let scopes = (0..self.scopes.len())
                    .filter(|&s| self.history[s].iter().any(|turn| lsns.contains(&turn.lsn)));
                let keys = self
                    .facts
                    .iter()
                    .filter(|(_, revs)| revs.iter().any(|(lsn, _)| lsns.contains(lsn)));
                (scopes.collect(), keys.map(|(key, _)| key.clone()).collect())
            }
        }
    }
    /// Reads every window and fact a forget batch names. Before the forget this fills the hot
    /// cache with turns it will hide. After the ack it proves they are hidden at once.
    fn check_touched(&self, engine: &Engine, index: usize) {
        let (scopes, keys) = self.touched(index);
        for scope in scopes {
            self.check_window(engine, scope, &format!("batch={index}"));
        }
        for key in keys {
            self.check_fact(engine, &key);
        }
    }
    /// The forget entries the engine holds and counts: sessions and keys that had a record
    /// when the forget committed, and (tenant, LSN) pairs at or below the cutoff.
    fn held_forgets(&self) -> [usize; 3] {
        let (mut sessions, mut keys, mut lsns) =
            (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        for (tenant, forgets) in &self.forgets {
            for (forget, selector, cutoff) in forgets {
                match selector {
                    Selector::Session { agent, session } => {
                        let written =
                            self.scopes.iter().zip(&self.history).any(|(scope, turns)| {
                                (&scope.tenant, &scope.agent, &scope.session)
                                    == (tenant, agent, session)
                                    && turns.first().is_some_and(|turn| turn.lsn < *forget)
                            });
                        if written {
                            sessions.insert((tenant, agent, session));
                        }
                    }
                    Selector::FactKey(key) => {
                        let revs = self.facts.get(&(tenant.clone(), key.clone()));
                        if revs.and_then(|revs| revs.first()).is_some_and(|(lsn, _)| lsn < forget) {
                            keys.insert((tenant, key));
                        }
                    }
                    Selector::Lsns(list) => {
                        lsns.extend(
                            list.iter().filter(|&lsn| lsn <= cutoff).map(|lsn| (tenant, *lsn)),
                        );
                    }
                }
            }
        }
        [sessions.len(), keys.len(), lsns.len()]
    }
    fn fact_key(&self, index: usize) -> Option<FactKey> {
        let batch = &self.batches[index];
        Some((self.scopes[batch.scope].tenant.clone(), batch.fact.as_ref()?.key.clone()))
    }
    /// The LSN of the key's current value, or 0 when it has none.
    fn current(&self, key: &FactKey) -> u64 {
        self.current_value(key).map_or(0, |(lsn, _)| lsn)
    }
    /// A conditional write conflicts only on a stale `if_lsn`, or behind an unresolved
    /// earlier write or forget to the same key in its group.
    fn assert_conflict(&self, index: usize, unresolved: &[usize]) {
        let key = self.fact_key(index).unwrap();
        let if_lsn = self.batches[index].fact.as_ref().unwrap().if_lsn;
        let forgets = |i: usize| {
            let batch = &self.batches[i];
            self.scopes[batch.scope].tenant == key.0
                && batch.forget.as_ref().is_some_and(|op| match &op.selector {
                    Selector::FactKey(name) => *name == key.1,
                    Selector::Lsns(_) => true,
                    Selector::Session { .. } => false,
                })
        };
        assert!(
            if_lsn.is_some_and(|lsn| lsn != self.current(&key))
                || unresolved
                    .iter()
                    .any(|&i| self.fact_key(i).as_ref() == Some(&key) || forgets(i)),
            "Conflict on a matching if_lsn: batch={index}"
        );
    }
    /// I9 and I10: the current value is the newest revision unless it is a tombstone or
    /// forgotten. History shows the newest `fact_history` revisions, forgotten ones as
    /// placeholders. Reads cost one tape read per returned entry that is not a placeholder.
    fn check_fact(&self, engine: &Engine, key: &FactKey) {
        let revs = self.facts.get(key).map_or(&[][..], Vec::as_slice);
        let before = engine.metrics().unwrap().fact_reads;
        let current = self.current_value(key);
        let newest: Vec<_> = revs
            .iter()
            .rev()
            .take(self.fact_history)
            .map(|(lsn, content)| self.revision(key, *lsn, content))
            .collect();
        let shown = newest.iter().filter(|rev| !rev.redacted).count();
        let reads = u64::from(current.is_some()) + shown as u64;
        let got = engine.fact_get(&key.0, &key.1).unwrap();
        assert_eq!(got.map(|fact| (fact.lsn, fact.content)), current, "I9: {key:?}");
        let history = engine.fact_history(&key.0, &key.1, usize::MAX).unwrap();
        assert_eq!(history.revisions, newest, "history: {key:?}");
        assert_eq!(history.truncated, revs.len() > self.fact_history, "truncated: {key:?}");
        assert_eq!(engine.metrics().unwrap().fact_reads - before, reads, "fact reads: {key:?}");
    }
    fn apply(&mut self, index: usize, receipt: Appended) {
        assert!(!receipt.deduplicated, "an unacknowledged batch must be a fresh commit");
        let previous = self.history.iter().flatten().map(|turn| turn.commit_time_ns).max();
        let previous = previous.unwrap_or(0).max(self.fact_time);
        if let Some(op) = self.batches[index].forget.clone() {
            self.fact_time = self.batches[index].time.max(previous + 1);
            self.batches[index].time = self.fact_time;
            let tenant = self.scopes[self.batches[index].scope].tenant.clone();
            let lsn = receipt.last_lsn;
            let cutoff = op.up_to_lsn.map_or(lsn, |up_to| up_to.min(lsn));
            self.forgets.entry(tenant).or_default().push((lsn, op.selector, cutoff));
            self.batches[index].receipt = Some(receipt);
            return;
        }
        if let Some(key) = self.fact_key(index) {
            self.fact_time = self.batches[index].time.max(previous + 1);
            self.batches[index].time = self.fact_time;
            let fact = self.batches[index].fact.clone().unwrap();
            if let Some(if_lsn) = fact.if_lsn {
                assert_eq!(self.current(&key), if_lsn, "committed on another LSN: batch={index}");
            }
            self.facts.entry(key).or_default().push((receipt.last_lsn, fact.content));
            self.batches[index].receipt = Some(receipt);
            return;
        }
        let b = &mut self.batches[index];
        b.time = b.time.max(previous + 1);
        let history = &mut self.history[b.scope];
        assert_eq!(
            receipt.seqs,
            (history.len() as u64 + 1..=history.len() as u64 + b.turns.len() as u64)
                .collect::<Vec<_>>()
        );
        assert_eq!(receipt.last_lsn - receipt.first_lsn + 1, b.turns.len() as u64);
        for (i, turn) in b.turns.iter().enumerate() {
            history.push(Turn {
                seq: receipt.seqs[i],
                lsn: receipt.first_lsn + i as u64,
                commit_time_ns: b.time,
                role: turn.role,
                content: turn.content.clone(),
                meta: turn.meta.clone(),
                redacted: false,
            });
        }
        b.receipt = Some(receipt);
    }
    fn verify(&self, engine: &Engine, rng: &mut Rng) {
        for (index, scope) in self.scopes.iter().enumerate() {
            let expected = &self.check_window(engine, index, "verify");
            let n = rng.below(expected.len() + 5);
            assert_eq!(
                engine.window(scope, WindowQuery::Last(n)).unwrap(),
                expected[expected.len().saturating_sub(n)..]
            );
            let seq = rng.below(expected.len() + 5) as u64;
            let limit = rng.below(20);
            let start = (seq.max(1) - 1) as usize;
            let end = expected.len().min(start.saturating_add(limit));
            let wanted = &expected[start.min(expected.len())..end];
            assert_eq!(engine.window(scope, WindowQuery::Since { seq, limit }).unwrap(), wanted);
        }
        assert_eq!(engine.metrics().unwrap().tenants, self.tenants.len());
        let metrics = engine.metrics().unwrap();
        let held =
            [metrics.forgotten_sessions, metrics.forgotten_fact_keys, metrics.forgotten_lsns];
        assert_eq!(held, self.held_forgets(), "forgotten sessions, fact keys and LSNs");
        for key in self.facts.keys() {
            self.check_fact(engine, key);
        }
        let prefixes: BTreeSet<&str> =
            FACT_KEYS.iter().flat_map(|key| (0..=key.len().min(3)).map(|n| &key[..n])).collect();
        for tenant in self.facts.keys().map(|(tenant, _)| tenant).collect::<BTreeSet<_>>() {
            let live: Vec<_> = self
                .facts
                .keys()
                .filter(|(owner, _)| owner == tenant)
                .filter_map(|fact_key| {
                    let (lsn, content) = self.current_value(fact_key)?;
                    Some((&fact_key.1, self.fact_batch(lsn).to_string(), content))
                })
                .collect();
            for prefix in &prefixes {
                let expected: Vec<_> =
                    live.iter().filter(|(key, ..)| key.starts_with(prefix)).collect();
                let (limit, max_bytes) =
                    (1 + rng.below(4), [0, 300, 2000, usize::MAX][rng.below(4)]);
                let (mut seen, mut after) = (0, None);
                loop {
                    let before = engine.metrics().unwrap().fact_reads;
                    let query = FactQuery { prefix: prefix.to_string(), after, limit, max_bytes };
                    let page = engine.fact_list(tenant, query).unwrap();
                    // The first `limit` remaining facts that fit the budget, at least one.
                    let (mut returned, mut bytes, mut cut) = (0, 0, false);
                    for (key, agent, content) in expected[seen..].iter().take(limit) {
                        bytes += key.len() + agent.len() + content.len();
                        if returned > 0 && bytes > max_bytes {
                            cut = true;
                            break;
                        }
                        returned += 1;
                    }
                    let got: Vec<_> =
                        page.facts.iter().map(|f| (&f.key, &f.agent, &f.content)).collect();
                    let expected: Vec<_> = expected
                        .iter()
                        .map(|(key, agent, content)| (*key, agent, content))
                        .collect();
                    let case = format!(
                        "tenant={tenant} prefix={prefix:?} limit={limit} max_bytes={max_bytes}"
                    );
                    assert_eq!(got, &expected[seen..seen + returned], "{case}");
                    let reads = engine.metrics().unwrap().fact_reads - before;
                    assert_eq!(reads, (returned + usize::from(cut)) as u64, "{case}");
                    seen += returned;
                    match page.next {
                        Some(next) if returned > 0 => after = Some(next),
                        next => {
                            assert_eq!((next, seen), (None, expected.len()), "{case}");
                            break;
                        }
                    }
                }
            }
        }
        self.check_records(engine, rng);
        // Half the time keeps the nightly 20,000-seed run affordable.
        if rng.below(2) == 0 {
            self.check_search(engine, rng);
        }
    }

    /// A full `records` walk of every tenant returns each turn and fact revision as written,
    /// newest first, and a forgotten one as a placeholder with its identifiers (I10).
    fn check_records(&self, engine: &Engine, rng: &mut Rng) {
        let tenants: BTreeSet<&String> = self.scopes.iter().map(|s| &s.tenant).collect();
        for tenant in tenants {
            let mut expected: Vec<(u64, Record)> = Vec::new();
            for (index, scope) in self.scopes.iter().enumerate() {
                if scope.tenant == *tenant {
                    expected.extend(self.window(index).into_iter().map(|turn| {
                        let (agent, session) = (scope.agent.clone(), scope.session.clone());
                        (turn.lsn, Record::Turn { agent, session, turn })
                    }));
                }
            }
            for (key, revs) in self.facts.iter().filter(|((owner, _), _)| owner == tenant) {
                expected.extend(revs.iter().map(|(lsn, content)| {
                    let revision = self.revision(key, *lsn, content);
                    (*lsn, Record::Fact { key: key.1.clone(), revision })
                }));
            }
            expected.sort_by_key(|(lsn, _)| std::cmp::Reverse(*lsn));
            let expected: Vec<Record> = expected.into_iter().map(|(_, record)| record).collect();
            let (limit, visits) = (1 + rng.below(64), [64, usize::MAX][rng.below(2)]);
            let (mut got, mut before) = (Vec::new(), None);
            loop {
                let query =
                    RecordQuery { before, limit, kind: None, max_bytes: usize::MAX, visits };
                let page = engine.records(tenant, query).unwrap();
                got.extend(page.records);
                match page.next {
                    Some(next) => before = Some(next),
                    None => break,
                }
            }
            assert_eq!(got, expected, "records: tenant={tenant} limit={limit} visits={visits}");
        }
    }

    /// Search, through the word index that simulations build inline, finds exactly the
    /// oracle's turns and current facts that hold the word, newest first.
    fn check_search(&self, engine: &Engine, rng: &mut Rng) {
        let has =
            |text: &[u8], word: &str| String::from_utf8_lossy(text).to_lowercase().contains(word);
        let tenants: Vec<&str> = self
            .scopes
            .iter()
            .map(|s| s.tenant.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        // 1 tenant per check keeps the nightly run short. Seeds cover the others. Every word
        // the content generator writes is searched, so hidden turns and facts are candidates.
        let tenant = tenants[rng.below(tenants.len())];
        for word in ["abab", "bba", "aaaa"] {
            let mut expected: Vec<(u64, Vec<u8>)> = (0..self.scopes.len())
                .filter(|&scope| self.scopes[scope].tenant == tenant)
                .flat_map(|scope| self.window(scope))
                .filter(|turn| !turn.redacted && has(&turn.content, word))
                .map(|turn| (turn.lsn, turn.content))
                .collect();
            for fact_key in self.facts.keys() {
                let (owner, key) = fact_key;
                if let Some((lsn, content)) = self.current_value(fact_key) {
                    if owner == tenant && (has(key.as_bytes(), word) || has(&content, word)) {
                        expected.push((lsn, content));
                    }
                }
            }
            expected.sort_by_key(|(lsn, _)| std::cmp::Reverse(*lsn));
            let (mut got, mut before) = (Vec::new(), None);
            loop {
                let query = SearchQuery {
                    query: word.into(),
                    before,
                    limit: 1 + rng.below(40),
                    max_bytes: usize::MAX,
                    visits: usize::MAX,
                };
                let page = engine.search(tenant, query).unwrap();
                got.extend(page.hits.into_iter().map(|hit| match hit {
                    Hit::Turn { turn, .. } => (turn.lsn, turn.content),
                    Hit::Fact(fact) => (fact.lsn, fact.content),
                }));
                match page.next {
                    Some(next) => before = Some(next),
                    None => break,
                }
            }
            assert_eq!(got, expected, "tenant={tenant} word={word}");
        }
    }
    // Only the unresolved batch may extend the acknowledged prefix, and only as a whole.
    fn recover(
        &mut self,
        disk: &SimDisk,
        clock: Arc<SimClock>,
        opts: &EngineOptions,
        pending: &[usize],
    ) -> u64 {
        let (tape, _) = Tape::open(Arc::new(disk.clone()), clock, "db", opts.tape.clone()).unwrap();
        let mut actual = vec![Vec::new(); self.scopes.len()];
        let mut transactions: BTreeMap<u64, Vec<(u64, u64)>> = BTreeMap::new();
        let mut tenant_names = BTreeMap::new();
        let mut facts: BTreeMap<FactKey, Revisions> = BTreeMap::new();
        let mut forgets = Forgets::new();
        let mut claimed = BTreeSet::new();
        let mut records = 0;
        for record in tape.scan(0) {
            let record = record.unwrap();
            records += 1;
            if record.kind == Kind::TenantCreate {
                let name = String::from_utf8(record.payload.to_vec()).unwrap();
                if let Some(id) = self.tenants.get(&name) {
                    assert_eq!(*id, record.tenant_id);
                }
                assert!(tenant_names.insert(record.tenant_id, name.clone()).is_none());
                self.tenants.insert(name, record.tenant_id);
                continue;
            }
            if record.kind == Kind::Forget {
                let forget = atticdb_core::forget::decode(&record.payload).unwrap();
                let tenant = tenant_names[&record.tenant_id].clone();
                // A keyless forget has no batch number, so it matches its payload. Identical
                // pending copies are told apart by claiming each once.
                let index = (0..self.batches.len())
                    .find(|&i| {
                        let batch = &self.batches[i];
                        batch.forget.as_ref().is_some_and(|op| {
                            (&op.selector, op.up_to_lsn) == (&forget.selector, forget.up_to_lsn)
                        }) && self.scopes[batch.scope].tenant == tenant
                            && batch.key == forget.idem_key
                            && match &batch.receipt {
                                Some(receipt) => receipt.last_lsn == record.lsn,
                                None => pending.contains(&i) && !claimed.contains(&i),
                            }
                    })
                    .unwrap_or_else(|| {
                        panic!("NotCommitted or unwritten forget recovered: lsn={}", record.lsn)
                    });
                claimed.insert(index);
                transactions.entry(record.txn).or_default().push((index as u64, record.lsn));
                let cutoff = forget.up_to_lsn.map_or(record.lsn, |lsn| lsn.min(record.lsn));
                forgets.entry(tenant).or_default().push((record.lsn, forget.selector, cutoff));
                continue;
            }
            if record.kind == Kind::Fact {
                let fact = atticdb_core::fact::decode(&record.payload).unwrap();
                let index: usize = fact.agent.parse().unwrap();
                let key = (tenant_names[&record.tenant_id].clone(), fact.key);
                let content = (!fact.tombstone).then_some(fact.content);
                let op = self.batches[index].fact.as_ref().unwrap();
                assert_eq!(self.fact_key(index).unwrap(), key);
                assert_eq!((&op.content, &self.batches[index].key), (&content, &fact.idem_key));
                assert!(
                    self.batches[index].receipt.is_some() || pending.contains(&index),
                    "NotCommitted or unwritten fact recovered: batch={index}"
                );
                transactions.entry(record.txn).or_default().push((index as u64, record.lsn));
                facts.entry(key).or_default().push((record.lsn, content));
                continue;
            }
            assert_eq!(record.kind, Kind::Turn);
            let decoded = decode(&record.payload).unwrap();
            let tenant = &tenant_names[&record.tenant_id];
            let scope = self
                .scopes
                .iter()
                .position(|s| {
                    &s.tenant == tenant
                        && s.agent == decoded.agent_id
                        && s.session == decoded.session_id
                })
                .unwrap();
            let batch = u64::from_le_bytes(decoded.turn.meta[..8].try_into().unwrap()) as usize;
            assert_eq!(self.batches[batch].scope, scope);
            assert_eq!(self.batches[batch].key, decoded.idem_key);
            assert!(
                self.batches[batch].receipt.is_some() || pending.contains(&batch),
                "NotCommitted or unwritten batch recovered: batch={batch}"
            );
            let turns = transactions.entry(record.txn).or_default();
            let input = &self.batches[batch].turns[turns.len()];
            assert_eq!(*input, decoded.turn);
            turns.push((batch as u64, record.lsn));
            actual[scope].push(Turn {
                seq: decoded.seq,
                lsn: record.lsn,
                commit_time_ns: record.commit_time_ns,
                role: decoded.turn.role,
                content: decoded.turn.content,
                meta: decoded.turn.meta,
                redacted: false,
            });
        }
        for (&txn, turns) in &transactions {
            let index = turns[0].0 as usize;
            assert!(turns.iter().all(|&(b, _)| b as usize == index));
            let batch = &self.batches[index];
            assert_eq!(turns.len(), batch.turns.len().max(1), "partial transaction");
            let receipt = Appended {
                txn,
                first_lsn: turns[0].1,
                last_lsn: turns.last().unwrap().1,
                seqs: actual[batch.scope]
                    .iter()
                    .filter(|t| t.lsn >= turns[0].1 && t.lsn <= turns.last().unwrap().1)
                    .map(|t| t.seq)
                    .collect(),
                deduplicated: false,
            };
            if let Some(expected) = &batch.receipt {
                assert_eq!(*expected, receipt);
            } else {
                assert!(pending.contains(&index));
                self.apply(index, receipt);
            }
        }
        assert_eq!(actual, self.history, "acknowledged data lost or unexpected turns recovered");
        assert_eq!(facts, self.facts, "acknowledged facts lost or unexpected facts recovered");
        assert_eq!(
            forgets, self.forgets,
            "acknowledged forgets lost or unexpected forgets recovered"
        );
        records
    }
}

/// A retry returns the original receipt and reports that it wrote nothing.
#[track_caller]
fn assert_deduplicated(original: &Appended, retried: &Appended) {
    assert!(retried.deduplicated, "a retry of an acknowledged key must report deduplicated");
    assert_eq!(*original, Appended { deduplicated: false, ..retried.clone() });
}

fn open(io: Arc<dyn Io>, clock: Arc<SimClock>, opts: &EngineOptions) -> Engine {
    Engine::open_with(io, clock, "db", opts.clone()).unwrap()
}

fn retry(engine: &Engine, oracle: &mut Oracle, index: usize) {
    let before = engine.metrics().unwrap().durable_lsn;
    match write(engine, oracle, index, false) {
        Err(error) if matches!(error.kind, ErrorKind::Conflict) => {
            assert!(oracle.batches[index].receipt.is_none(), "a committed retry key conflicted");
            assert_eq!(error.outcome, Outcome::NotCommitted);
            oracle.assert_conflict(index, &[]);
        }
        result => {
            let receipt = result.unwrap();
            if let Some(expected) = &oracle.batches[index].receipt {
                assert_deduplicated(expected, &receipt);
                assert_eq!(engine.metrics().unwrap().durable_lsn, before);
            } else {
                oracle.apply(index, receipt);
            }
        }
    }
    if let Some(key) = oracle.fact_key(index) {
        oracle.check_fact(engine, &key);
        return;
    }
    if oracle.batches[index].forget.is_some() {
        oracle.check_touched(engine, index);
        return;
    }
    let scope = oracle.batches[index].scope;
    assert_eq!(
        engine.window(&oracle.scopes[scope], WindowQuery::Last(usize::MAX)).unwrap(),
        oracle.window(scope),
        "an acknowledged retry must be immediately readable"
    );
}

/// The fact write a batch names. `changed` alters the payload but keeps the retry key.
fn new_fact(op: &FactOp, index: usize, changed: bool) -> NewFact {
    let agent = if changed { format!("{index}!") } else { index.to_string() };
    let content = op.content.clone().unwrap_or_default();
    NewFact { key: op.key.clone(), content, meta: vec![], agent }
}

fn fact_receipt(written: FactWritten) -> Appended {
    let FactWritten { txn, lsn, deduplicated } = written;
    Appended { txn, first_lsn: lsn, last_lsn: lsn, seqs: vec![], deduplicated }
}

/// A forget receipt's cutoff is `up_to_lsn` when lower than its LSN, on retries too.
#[track_caller]
fn forget_receipt(forgotten: Forgotten, up_to_lsn: Option<u64>) -> Appended {
    let Forgotten { txn, lsn, cutoff, deduplicated } = forgotten;
    assert_eq!(cutoff, up_to_lsn.map_or(lsn, |up_to| up_to.min(lsn)), "cutoff of {lsn}");
    Appended { txn, first_lsn: lsn, last_lsn: lsn, seqs: vec![], deduplicated }
}

/// The forget a batch names. `changed` alters the cutoff but keeps the retry key.
fn forget_args(op: &ForgetOp, changed: bool) -> (Selector, Option<u64>) {
    let up_to_lsn =
        if changed { Some(op.up_to_lsn.map_or(1, |lsn| lsn + 1)) } else { op.up_to_lsn };
    (op.selector.clone(), up_to_lsn)
}

/// Runs a batch as a turn append, a fact write or a forget.
fn write(
    engine: &Engine,
    oracle: &Oracle,
    index: usize,
    changed: bool,
) -> std::result::Result<Appended, EngineError> {
    let batch = &oracle.batches[index];
    let (scope, key) = (&oracle.scopes[batch.scope], batch.key.as_deref());
    if let Some(op) = &batch.forget {
        let (selector, up_to_lsn) = forget_args(op, changed);
        let forgotten = engine.forget(&scope.tenant, selector, key, up_to_lsn);
        return forgotten.map(|forgotten| forget_receipt(forgotten, up_to_lsn));
    }
    let Some(op) = &batch.fact else {
        let mut turns = batch.turns.clone();
        if changed {
            turns[0].content.push(0xff);
        }
        return engine.append(scope, turns, key);
    };
    let fact = new_fact(op, index, changed);
    match op.content {
        Some(_) => engine.fact_put(&scope.tenant, fact, key, op.if_lsn),
        None => engine.fact_delete(&scope.tenant, &fact.key, &fact.agent, key, op.if_lsn),
    }
    .map(fact_receipt)
}

type Queued<'a> = Box<dyn FnOnce() -> std::result::Result<Appended, EngineError> + 'a>;

/// Queues a batch for the next inline step. Its tenant must exist.
fn enqueue<'a>(engine: &'a Engine, oracle: &Oracle, index: usize, changed: bool) -> Queued<'a> {
    let batch = &oracle.batches[index];
    let (scope, key) = (&oracle.scopes[batch.scope], batch.key.as_deref());
    if let Some(op) = &batch.forget {
        let (selector, up_to_lsn) = forget_args(op, changed);
        let reply = engine.enqueue_forget_sim(&scope.tenant, selector, key, up_to_lsn).unwrap();
        return Box::new(move || reply.finish().map(|f| forget_receipt(f, up_to_lsn)));
    }
    let Some(op) = &batch.fact else {
        let mut turns = batch.turns.clone();
        if changed {
            turns[0].content.push(0xff);
        }
        let reply = engine.enqueue_sim(scope, turns, key).unwrap();
        return Box::new(move || reply.finish());
    };
    let fact = new_fact(op, index, changed);
    let reply =
        engine.enqueue_fact_sim(&scope.tenant, fact, op.content.is_none(), key, op.if_lsn).unwrap();
    Box::new(move || reply.finish().map(fact_receipt))
}

/// A turn batch, or with `facts` a fact put, delete or conditional put one time in three.
/// With `forgets` too, about 1 batch in 10 is a forget.
fn random_batch(
    rng: &mut Rng,
    index: usize,
    scope: usize,
    time: u64,
    facts: Option<&Oracle>,
    forgets: bool,
) -> Batch {
    let key = if rng.below(3) == 0 { None } else { Some(format!("key-{index}")) };
    if let Some(oracle) = facts.filter(|_| forgets && rng.below(10) == 0) {
        let (scope, forget) = random_forget(rng, oracle, scope);
        let forget = Some(forget);
        return Batch { scope, turns: vec![], key, receipt: None, time, fact: None, forget };
    }
    if let Some(oracle) = facts.filter(|_| rng.below(3) == 0) {
        let fact_key = FACT_KEYS[rng.below(FACT_KEYS.len())].to_owned();
        let current = oracle.current(&(oracle.scopes[scope].tenant.clone(), fact_key.clone()));
        let size = [0, 1, 31, 256, 1024][rng.below(5)];
        // Text half the time, so search finds facts too.
        let content = (rng.below(4) > 0).then(|| rng.content(size));
        let stale = Some(rng.below(current as usize + 2) as u64);
        let if_lsn = [None, Some(current), stale][rng.below(3)];
        let fact = Some(FactOp { key: fact_key, content, if_lsn });
        return Batch { scope, turns: vec![], key, receipt: None, time, fact, forget: None };
    }
    let turns = (0..1 + rng.below(4))
        .map(|_| {
            let size = [0, 1, 31, 256, 1024, 8192][rng.below(6)];
            let meta = rng.below(64);
            let mut metadata = (index as u64).to_le_bytes().to_vec();
            metadata.extend(rng.bytes(meta));
            NewTurn {
                role: [Role::System, Role::User, Role::Assistant, Role::Tool][rng.below(4)],
                content: rng.content(size),
                meta: metadata,
            }
        })
        .collect();
    Batch { scope, turns, key, receipt: None, time, fact: None, forget: None }
}

/// A forget of a session, a fact key, or 1 to 8 acknowledged LSNs, sometimes with
/// `up_to_lsn`. Half of the session and key forgets name the newest such write, which may be
/// queued earlier in the same group. 1 forget in 4 crosses tenants: it runs in `scope`'s
/// tenant, with LSNs of every tenant, or a session or key named after any tenant's write.
/// Names repeat across tenants, so only the forget's own tenant may lose records.
/// Returns the scope whose tenant runs the forget.
fn random_forget(rng: &mut Rng, oracle: &Oracle, scope: usize) -> (usize, ForgetOp) {
    let newest = |fact: bool| {
        let mut writes = oracle.batches.iter().rev().filter(|b| b.forget.is_none());
        writes.find(|b| b.fact.is_some() == fact)
    };
    let tenant = &oracle.scopes[scope].tenant;
    let other = rng.below(4) == 0;
    let turns = oracle
        .scopes
        .iter()
        .zip(&oracle.history)
        .flat_map(|(scope, turns)| turns.iter().map(move |turn| (&scope.tenant, turn.lsn)));
    let facts =
        oracle.facts.iter().flat_map(|((owner, _), revs)| revs.iter().map(move |r| (owner, r.0)));
    let pool: Vec<u64> = turns
        .chain(facts)
        .filter(|(owner, _)| other || *owner == tenant)
        .map(|(_, lsn)| lsn)
        .collect();
    let (target, selector) = match rng.below(3) {
        0 if !pool.is_empty() => {
            let lsns = (0..1 + rng.below(8)).map(|_| pool[rng.below(pool.len())]).collect();
            (scope, Selector::Lsns(lsns))
        }
        1 => match newest(true).filter(|_| rng.below(2) == 0) {
            Some(batch) => {
                (batch.scope, Selector::FactKey(batch.fact.as_ref().unwrap().key.clone()))
            }
            None => (scope, Selector::FactKey(FACT_KEYS[rng.below(FACT_KEYS.len())].into())),
        },
        _ => {
            let target = match newest(false).filter(|_| rng.below(2) == 0) {
                Some(batch) => batch.scope,
                None => rng.below(oracle.scopes.len()),
            };
            let Scope { agent, session, .. } = oracle.scopes[target].clone();
            (target, Selector::Session { agent, session })
        }
    };
    let target = if other { scope } else { target };
    let newest_lsn = oracle.batches.iter().filter_map(|b| b.receipt.as_ref()).map(|r| r.last_lsn);
    let max = newest_lsn.max().unwrap_or(0) as usize;
    let up_to_lsn = (rng.below(3) == 0).then(|| 1 + rng.below(max + 2) as u64);
    (target, ForgetOp { selector, up_to_lsn })
}

fn corrupt_page_checksum(disk: &SimDisk, rng: &mut Rng) {
    let mut pages = Vec::new();
    for path in disk
        .list(Path::new("db"))
        .unwrap()
        .into_iter()
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with("ckpt-"))
    {
        let file = disk.open(&path).unwrap();
        let mut offset = 0;
        while offset < disk.len(file).unwrap() {
            let mut header = [0; 9];
            assert_eq!(disk.read_at(file, offset, &mut header).unwrap(), header.len());
            let len = u32::from_le_bytes(header[4..8].try_into().unwrap());
            if matches!(header[8], 2 | 4..=9) {
                pages.push((path.clone(), offset));
            }
            offset += u64::from(len);
        }
        disk.close(file).unwrap();
    }
    assert!(!pages.is_empty());
    let (path, offset) = &pages[rng.below(pages.len())];
    let file = disk.open(path).unwrap();
    disk.flip_bit_at(file, offset + rng.below(4) as u64, rng.below(8) as u8).unwrap();
    disk.close(file).unwrap();
}

fn run_seed(seed: u64) -> Vec<Vec<Turn>> {
    let plan = CapacityPlan::new(seed);
    match std::panic::catch_unwind(|| run_seed_with_capacity(seed, &plan)) {
        Ok(history) => history,
        Err(error) => {
            eprintln!("engine simulation seed={seed} fault_plan={plan:?}");
            std::panic::resume_unwind(error);
        }
    }
}

fn run_seed_with_capacity(seed: u64, plan: &CapacityPlan) -> Vec<Vec<Turn>> {
    let mut rng = Rng(seed);
    let mut opts = EngineOptions {
        tape: TapeOptions {
            segment_size: 64 * 1024,
            inline_sim: true,
            lsn_index_bytes: if seed.is_multiple_of(3) { 12 * 1024 } else { 8 * 1024 * 1024 },
            ..TapeOptions::default()
        },
        hot_page_bytes: if seed.is_multiple_of(2) { 4096 } else { 1024 * 1024 },
        checkpoint_bytes: 4096,
        fact_history: 1 + (seed % 3) as usize,
        // Small stores: walking fewer records first makes search use the word index.
        search_walk_first: [0, 16, 1024][(seed % 3) as usize],
        ..EngineOptions::default()
    };
    let clock = Arc::new(SimClock::default());
    let mut io = Arc::new(HookIo::sim(seed));
    let mut engine = open(io.clone(), clock.clone(), &opts);
    let mut oracle = Oracle::new();
    oracle.fact_history = opts.fact_history;
    for generation in 0..3 {
        // Later reopens switch modes, so stores mix plain and compressed turns and migrate.
        let modes = [CompressionMode::Auto, CompressionMode::Off, CompressionMode::On];
        opts.compression.mode = modes[(seed as usize + generation) % 3];
        for step in 0..40 {
            clock.advance(1 + rng.below(2_000_000_000) as u64);
            // Seeded points replace the checkpoint timer. Memory-pressure flushes remain automatic.
            if rng.below(7) == 0 {
                engine.checkpoint().unwrap();
            }
            if !oracle.batches.is_empty() && rng.below(5) == 0 {
                let index = rng.below(oracle.batches.len());
                if oracle.batches[index].receipt.is_some() && oracle.batches[index].key.is_some() {
                    retry(&engine, &mut oracle, index);
                    let before = engine.metrics().unwrap().durable_lsn;
                    let error = write(&engine, &oracle, index, true).unwrap_err();
                    assert_eq!(error.outcome, Outcome::NotCommitted);
                    assert!(matches!(error.kind, ErrorKind::IdempotencyConflict));
                    assert_eq!(before, engine.metrics().unwrap().durable_lsn);
                }
            }
            let scope = if rng.below(3) == 0 { 0 } else { rng.below(oracle.scopes.len()) };
            let index = oracle.batches.len();
            let batch = random_batch(&mut rng, index, scope, clock.now_ns(), Some(&oracle), true);
            oracle.batches.push(batch);
            let forget = oracle.batches[index].forget.is_some();
            if forget {
                // Fills the hot cache with turns the forget will hide.
                oracle.check_touched(&engine, index);
            }
            let fault = step == 12 || step == 28;
            let failing = fault.then(|| fail_sync(&io, step == 28));
            let cut = step == 39;
            let take = cut.then(|| cut_at(&io, 1 + rng.below(6)));
            let result = write(&engine, &oracle, index, false);
            let image = take.and_then(|take| take());
            if image.is_some() {
                if let Ok(receipt) = &result {
                    let mut after_ack = oracle.clone();
                    after_ack.apply(index, receipt.clone());
                    after_ack.recover(&io.inner.crash().unwrap(), clock.clone(), &opts, &[]);
                }
            }
            let mut pending = Vec::new();
            if image.is_some() {
                if result.is_ok() || result.as_ref().is_err_and(|e| e.outcome == Outcome::Unknown) {
                    pending.push(index);
                }
            } else {
                match result {
                    Ok(receipt) => {
                        oracle.apply(index, receipt);
                        if forget {
                            oracle.check_touched(&engine, index);
                        }
                    }
                    Err(error) if error.outcome == Outcome::Unknown => pending.push(index),
                    Err(error) if matches!(error.kind, ErrorKind::Conflict) => {
                        assert_eq!(error.outcome, Outcome::NotCommitted);
                        oracle.assert_conflict(index, &[]);
                    }
                    Err(error) => {
                        assert_eq!(error.outcome, Outcome::NotCommitted);
                        assert!(matches!(
                            error.kind,
                            ErrorKind::Storage(_) | ErrorKind::ResourceExhausted("hot_pages")
                        ));
                    }
                }
            }
            if fault || cut {
                failing.iter().for_each(|armed| armed.store(0, Ordering::SeqCst));
                // A captured image excludes graceful shutdown, unlike live reopen.
                let next = image.unwrap_or_else(|| {
                    if cut {
                        io.inner.crash().unwrap()
                    } else {
                        io.inner.clone()
                    }
                });
                drop(engine);
                oracle.recover(&next, clock.clone(), &opts, &pending);
                io = Arc::new(HookIo::new(next));
                engine = open(io.clone(), clock.clone(), &opts);
                oracle.verify(&engine, &mut rng);
                // Unknown retries must either return the original receipt or install exactly once.
                for index in pending
                    .into_iter()
                    .filter(|&i| oracle.batches[i].key.is_some())
                    .collect::<Vec<_>>()
                {
                    oracle.batches[index].time = if oracle.batches[index].receipt.is_none() {
                        clock.now_ns()
                    } else {
                        oracle.batches[index].time
                    };
                    retry(&engine, &mut oracle, index);
                    retry(&engine, &mut oracle, index);
                }
            }
            if rng.below(8) == 0 {
                // Discover newly acknowledged tenant IDs through the independent tape scan at reopen.
                let expected = oracle.window(scope);
                assert_eq!(
                    engine.window(&oracle.scopes[scope], WindowQuery::Last(7)).unwrap(),
                    expected[expected.len().saturating_sub(7)..]
                );
            }
            assert!(engine.metrics().unwrap().hot_page_bytes <= opts.hot_page_bytes);
        }
        // Cut checkpoint installation independently from append, including directory persistence.
        let take = cut_at(&io, 1 + rng.below(14));
        engine.checkpoint().unwrap();
        let next = take().unwrap_or_else(|| io.inner.crash().unwrap());
        drop(engine);
        oracle.recover(&next, clock.clone(), &opts, &[]);
        io = Arc::new(HookIo::new(next));
        engine = open(io.clone(), clock.clone(), &opts);
        oracle.verify(&engine, &mut rng);
        engine.checkpoint().unwrap();
        drop(engine);
        // Checkpoints are derived data. Corrupt a referenced file and demand full replay.
        corrupt_page_checksum(&io.inner, &mut rng);
        let records = oracle.recover(&io.inner, clock.clone(), &opts, &[]);
        engine = open(io.clone(), clock.clone(), &opts);
        let metrics = engine.metrics().unwrap();
        assert!(metrics.checkpoint_warnings > 0, "generation={generation}");
        assert_eq!(metrics.replayed_records, records);
        oracle.verify(&engine, &mut rng);
        for index in 0..oracle.batches.len() {
            if oracle.batches[index].receipt.is_some()
                && oracle.batches[index].key.is_some()
                && rng.below(5) == 0
            {
                retry(&engine, &mut oracle, index);
            }
        }
    }
    let mut history = oracle.history;
    history.extend(grouped_seed(seed, plan));
    history
}

/// One keyed turn, unconditional fact put or forget. Turns and facts carry 512 to 2559 content
/// bytes.
fn enqueue_keyed_batch<'a>(
    engine: &'a Engine,
    oracle: &mut Oracle,
    rng: &mut Rng,
    clock: &SimClock,
) -> (usize, Queued<'a>) {
    let index = oracle.batches.len();
    let scope = rng.below(4);
    let mut batch = random_batch(rng, index, scope, clock.now_ns(), Some(oracle), true);
    let len = 512 + rng.below(2048);
    let content = rng.bytes(len);
    match &mut batch.fact {
        Some(fact) => (fact.content, fact.if_lsn) = (Some(content), None),
        None if batch.forget.is_some() => {}
        None => batch.turns = vec![NewTurn { content, ..batch.turns[0].clone() }],
    }
    batch.key = Some(format!("key-{index}"));
    oracle.batches.push(batch);
    (index, enqueue(engine, oracle, index, false))
}

fn capacity_faults(
    engine: Arc<Engine>,
    io: &mut Arc<HookIo>,
    clock: &Arc<SimClock>,
    opts: &EngineOptions,
    oracle: &mut Oracle,
    rng: &mut Rng,
    plan: &CapacityPlan,
) -> Arc<Engine> {
    let snapshot = |engine: &Engine, scopes: &[Scope]| {
        let metrics = engine.metrics().unwrap();
        let turns = scopes
            .iter()
            .map(|scope| engine.window(scope, WindowQuery::Last(usize::MAX)).unwrap())
            .collect::<Vec<_>>();
        (
            metrics.durable_lsn,
            metrics.tenants,
            metrics.dedup_entries,
            metrics.dedup_window_effective,
            (metrics.facts, metrics.fact_revisions),
            turns,
        )
    };
    let before = snapshot(&engine, &oracle.scopes);
    let mut txn =
        oracle.batches.iter().filter_map(|b| b.receipt.as_ref().map(|r| r.txn)).max().unwrap();
    let mut indices = Vec::new();
    let mut queued = Vec::new();
    for _ in 0..2 + rng.below(3) {
        let (index, reply) = enqueue_keyed_batch(&engine, oracle, rng, clock);
        queued.push(reply);
        indices.push(index);
    }
    plan.limit(&io.inner);
    let segment_writes = io.tally(|op| (matches!(op, Op::Write(..)) && op.is_segment()) as u64);
    engine.step_sim().unwrap();
    assert_eq!(segment_writes.load(Ordering::SeqCst), 0, "a floor rejection wrote to a segment");
    for reply in queued {
        let error = reply().unwrap_err();
        assert!(matches!(error.kind, ErrorKind::Storage(Error::DiskFull)));
        assert_eq!(error.outcome, Outcome::NotCommitted);
    }
    assert_eq!(engine.health(), Health::ReadOnly, "a clean floor hit must not poison");
    // The oldest turn a forget does not hide, so the read reaches the tape.
    let (scope, oldest) = (0..oracle.scopes.len())
        .filter_map(|scope| {
            oracle.window(scope).into_iter().find(|t| !t.redacted).map(|t| (scope, t))
        })
        .min_by_key(|(_, turn)| turn.lsn)
        .unwrap();
    let capacity = io.inner.total_bytes(Path::new("db")).unwrap();
    let free = io.inner.available_bytes(Path::new("db")).unwrap();
    io.inner.set_capacity(capacity - free).unwrap();
    let reloads = engine.metrics().unwrap().tape_index.page_reloads;
    assert_eq!(
        engine
            .window(&oracle.scopes[scope], WindowQuery::Since { seq: oldest.seq, limit: 1 })
            .unwrap_or_else(|error| panic!("read-only cold read must not allocate: {error:?}")),
        vec![oldest],
        "acknowledged turn must remain readable with no free space"
    );
    if opts.tape.lsn_index_bytes == 4224 {
        assert!(
            engine.metrics().unwrap().tape_index.page_reloads > reloads,
            "sampled a cold index page"
        );
    }
    for key in oracle.facts.keys() {
        oracle.check_fact(&engine, key);
    }
    assert_eq!(io.inner.available_bytes(Path::new("db")).unwrap(), 0);
    assert_eq!(engine.health(), Health::ReadOnly, "cold reads must not clear read-only");
    io.inner.set_capacity(capacity).unwrap();
    if let Some(index) =
        oracle.batches.iter().position(|batch| batch.receipt.is_some() && batch.key.is_some())
    {
        retry(&engine, oracle, index);
        assert_eq!(engine.health(), Health::ReadOnly, "a committed retry does not recheck space");
    }
    assert_eq!(
        snapshot(&engine, &oracle.scopes),
        before,
        "a floor hit changed engine state or dedup"
    );

    plan.free(&io.inner);
    let mut lsn = before.0;
    for index in indices {
        retry(&engine, oracle, index);
        let receipt = oracle.batches[index].receipt.as_ref().unwrap();
        txn += 1;
        assert_eq!(receipt.txn, txn, "a floor rejection consumed a transaction");
        assert_eq!(receipt.first_lsn, lsn + 1, "a floor rejection consumed an LSN");
        assert_eq!(
            engine.metrics().unwrap().durable_lsn,
            receipt.last_lsn,
            "a rejected key returned a receipt without a durable write"
        );
        lsn = receipt.last_lsn;
        assert_eq!(engine.health(), Health::Ok, "freed space must clear read-only");
        retry(&engine, oracle, index);
    }
    // Retained keys must still return their original receipts after the rejection.
    for index in 0..oracle.batches.len() {
        if oracle.batches[index].receipt.is_some() && oracle.batches[index].key.is_some() {
            retry(&engine, oracle, index);
        }
    }

    let mut queued = Vec::new();
    for _ in 0..2 + rng.below(3) {
        queued.push(enqueue_keyed_batch(&engine, oracle, rng, clock));
    }
    storage_full(io, plan);
    let take = cut_at(io, plan.cut);
    let full_before = io.inner.storage_full_returns().unwrap();
    engine.step_sim().unwrap();
    assert_eq!(engine.health(), Health::Poisoned, "post-prepare StorageFull must poison");
    assert_eq!(io.inner.storage_full_returns().unwrap() - full_before, 1);
    let image = take();
    let mut pending = Vec::new();
    let mut post_return_pending = Vec::new();
    for (position, (index, reply)) in queued.into_iter().enumerate() {
        let error = reply().unwrap_err();
        let (class, outcomes) = plan.expected(position);
        assert!(
            match &error.kind {
                ErrorKind::Storage(error) => class.matches(error),
                ErrorKind::Poisoned => class == CapacityError::Poisoned,
                _ => false,
            },
            "{error:?}"
        );
        assert!(outcomes.contains(&error.outcome), "{error:?}");
        if error.outcome == Outcome::Unknown {
            post_return_pending.push(index);
        }
        if image.is_some() || error.outcome == Outcome::Unknown {
            pending.push(index);
        }
    }
    let crashed = io.inner.crash().unwrap();
    drop(engine);
    if image.is_some() {
        oracle.clone().recover(&crashed, clock.clone(), opts, &post_return_pending);
    }
    let next = image.unwrap_or(crashed);
    oracle.recover(&next, clock.clone(), opts, &pending);
    next.set_capacity(SimDisk::DEFAULT_CAPACITY).unwrap();
    *io = Arc::new(HookIo::new(next));
    let engine = Arc::new(open(io.clone(), clock.clone(), opts));
    assert_eq!(engine.health(), Health::Ok);
    oracle.verify(&engine, rng);
    for index in pending {
        if oracle.batches[index].receipt.is_none() {
            oracle.batches[index].time = clock.now_ns();
        }
        retry(&engine, oracle, index);
        retry(&engine, oracle, index);
    }
    engine
}

fn grouped_seed(seed: u64, plan: &CapacityPlan) -> Vec<Vec<Turn>> {
    let mut rng = Rng(seed ^ 0x47b3_6f21_91e2_450a);
    let generations = 3;
    let capacity_generation = rng.below(generations);
    let opts = EngineOptions {
        tape: TapeOptions {
            segment_size: 64 * 1024,
            inline_sim: true,
            free_space_floor: Some(plan.floor),
            lsn_index_bytes: if seed.is_multiple_of(3) { 4224 } else { 8 * 1024 * 1024 },
            ..TapeOptions::default()
        },
        checkpoint_bytes: 8192,
        // Seeds with a tiny tape index prove cold reads reach it, so they skip the hot cache.
        hot_cache_bytes: if seed.is_multiple_of(3) { 0 } else { 64 * 1024 * 1024 },
        ..EngineOptions::default()
    };
    let clock = Arc::new(SimClock::default());
    let mut io = Arc::new(HookIo::sim(seed));
    let mut engine = Arc::new(open(io.clone(), clock.clone(), &opts));
    let mut oracle = Oracle::new();
    for scope in 0..4 {
        let index = oracle.batches.len();
        let mut batch = random_batch(&mut rng, index, scope, clock.now_ns(), None, false);
        batch.turns.truncate(1);
        let receipt = engine
            .append(&oracle.scopes[scope], batch.turns.clone(), batch.key.as_deref())
            .unwrap();
        oracle.batches.push(batch);
        oracle.apply(index, receipt);
    }

    fn frame_faults(seed: u64) {
        use atticdb_tape::sim::frame;
        for proof in [false, true] {
            let disk = Arc::new(SimDisk::new(seed));
            let opts = EngineOptions {
                tape: TapeOptions {
                    segment_size: 4096,
                    inline_sim: true,
                    ..TapeOptions::default()
                },
                checkpoint_bytes: u64::MAX,
                checkpoint_interval: std::time::Duration::from_secs(3600),
                ..EngineOptions::default()
            };
            let clock = Arc::new(SimClock::new(1000));
            let engine = Engine::open_with(disk.clone(), clock, "db", opts.clone()).unwrap();
            let scope = one_scope();
            for index in 0..if proof { 3 } else { 1 } {
                engine
                    .append(
                        &scope,
                        vec![NewTurn { role: Role::User, content: vec![index], meta: vec![] }],
                        Some(&format!("key-{index}")),
                    )
                    .unwrap();
            }
            let image = Arc::new(disk.crash().unwrap());
            drop(engine);
            let path = Path::new("db/seg-0000000000000001.zdb");
            let file = image.open(path).unwrap();
            let mut bytes = vec![0; 4096];
            image.read_at(file, 0, &mut bytes).unwrap();
            let mut offset = 0;
            while let Ok((record, used)) = frame::decode(&bytes[offset..], 1, offset as u64) {
                if record.header.kind == Kind::Turn {
                    break;
                }
                offset += used;
            }
            image
                .flip_bit_at(
                    file,
                    (offset + if seed.is_multiple_of(2) { 4 } else { 40 }) as u64,
                    (seed % 8) as u8,
                )
                .unwrap();
            image.close(file).unwrap();
            let result = Engine::open_with(image, Arc::new(SimClock::new(0)), "db", opts);
            if proof {
                assert!(
                    matches!(
                        result,
                        Err(atticdb_core::EngineError {
                            kind: ErrorKind::Storage(Error::CorruptionDetected { lsn: 2, .. }),
                            ..
                        })
                    ),
                    "seed={seed}"
                );
            } else {
                let engine = result.unwrap();
                assert_eq!(engine.metrics().unwrap().durable_lsn, 0);
                assert!(engine.window(&scope, WindowQuery::Last(10)).unwrap().is_empty());
            }
        }
    }
    frame_faults(seed);
    for generation in 0..generations {
        for group in 0..8 {
            clock.advance(1 + rng.below(2_000_000_000) as u64);
            if generation == capacity_generation && group == plan.delay + 1 {
                engine =
                    capacity_faults(engine, &mut io, &clock, &opts, &mut oracle, &mut rng, plan);
            }
            let count = 2 + rng.below(3);
            let mut queued = Vec::new();
            for position in 0..count {
                let index = oracle.batches.len();
                let scope = rng.below(oracle.scopes.len());
                // Position 0 stays a turn batch: its duplicate and changed copies follow it.
                let facts = (position > 0).then_some(&oracle);
                let mut batch = random_batch(&mut rng, index, scope, clock.now_ns(), facts, true);
                if position == 0 {
                    batch.key = Some(format!("key-{index}"));
                }
                oracle.batches.push(batch);
                queued.push((index, false, enqueue(&engine, &oracle, index, false)));
                if position == 0 {
                    queued.push((index, false, enqueue(&engine, &oracle, index, false)));
                    queued.push((index, true, enqueue(&engine, &oracle, index, true)));
                }
            }
            // Fills the hot cache with turns the queued forgets will hide.
            let forgets: Vec<usize> = queued
                .iter()
                .map(|(index, ..)| *index)
                .filter(|&index| oracle.batches[index].forget.is_some())
                .collect();
            forgets.iter().for_each(|&index| oracle.check_touched(&engine, index));
            let fault = group == 6;
            let cut = group == 7;
            let checkpoint_fired = Arc::new(AtomicBool::new(false));
            let metrics = engine.metrics().unwrap();
            let boundary = metrics.durable_lsn;
            let writer_in_checkpoint = !fault
                && !cut
                && rng.below(2) == 0
                && metrics.durable_lsn > metrics.checkpoint_watermark;
            let mut take = None;
            let armed = if fault {
                Some(fail_sync(&io, generation % 2 == 1))
            } else if cut {
                take = Some(cut_at(&io, 1 + rng.below(4)));
                None
            } else {
                let weak = Arc::downgrade(&engine);
                let fired = checkpoint_fired.clone();
                let checkpoint = move || {
                    let engine = weak.upgrade().unwrap();
                    if writer_in_checkpoint {
                        engine.step_sim().unwrap();
                    } else {
                        assert_eq!(engine.metrics().unwrap().durable_lsn, boundary);
                        assert_eq!(engine.checkpoint().unwrap(), boundary);
                    }
                    fired.store(true, Ordering::SeqCst);
                };
                // A conditional fact can conflict and write nothing.
                let conditional = queued
                    .iter()
                    .filter(|(i, _, _)| {
                        oracle.batches[*i].fact.as_ref().is_some_and(|f| f.if_lsn.is_some())
                    })
                    .count();
                Some(after_nth(&io, 1 + rng.below(count - conditional), checkpoint))
            };
            if writer_in_checkpoint {
                assert_eq!(engine.checkpoint().unwrap(), boundary);
                assert!(engine.metrics().unwrap().durable_lsn > boundary);
            } else {
                engine.step_sim().unwrap();
            }
            let results = queued
                .into_iter()
                .map(|(index, conflict, reply)| (index, conflict, reply()))
                .collect::<Vec<_>>();
            if !fault && !cut {
                assert!(checkpoint_fired.load(Ordering::SeqCst));
            }
            armed.iter().for_each(|armed| armed.store(0, Ordering::SeqCst));
            let image = take.and_then(|take| take());
            if image.is_some() {
                let mut after_ack = oracle.clone();
                let mut uncertain = Vec::new();
                for (index, conflict, result) in &results {
                    if *conflict {
                        continue;
                    }
                    if let Ok(receipt) = result {
                        if let Some(original) = &after_ack.batches[*index].receipt {
                            assert_deduplicated(original, receipt);
                        } else {
                            after_ack.apply(*index, receipt.clone());
                        }
                    } else if result.as_ref().is_err_and(|e| e.outcome == Outcome::Unknown) {
                        uncertain.push(*index);
                    }
                }
                // Acknowledgements after the earlier cut must survive a later, post-return crash.
                after_ack.recover(&io.inner.crash().unwrap(), clock.clone(), &opts, &uncertain);
            }
            let mut pending = Vec::new();
            for (index, conflict, result) in results {
                if conflict {
                    let error = result.unwrap_err();
                    assert_eq!(error.outcome, Outcome::NotCommitted);
                    if !fault {
                        assert!(matches!(error.kind, ErrorKind::IdempotencyConflict));
                    }
                    continue;
                }
                match result {
                    Ok(receipt) if image.is_none() => {
                        if let Some(original) = &oracle.batches[index].receipt {
                            assert_deduplicated(original, &receipt);
                        } else {
                            oracle.apply(index, receipt);
                        }
                    }
                    Ok(_) => pending.push(index),
                    Err(error) if error.outcome == Outcome::Unknown => pending.push(index),
                    Err(error) => {
                        assert_eq!(error.outcome, Outcome::NotCommitted);
                        if matches!(error.kind, ErrorKind::Conflict) {
                            oracle.assert_conflict(index, &pending);
                        }
                    }
                }
            }
            pending.sort_unstable();
            pending.dedup();
            // Acknowledged forgets hide at once, cached turns included.
            for &index in forgets.iter().filter(|&&i| oracle.batches[i].receipt.is_some()) {
                oracle.check_touched(&engine, index);
            }
            if fault || cut {
                let next = image.unwrap_or_else(|| {
                    if cut {
                        io.inner.crash().unwrap()
                    } else {
                        io.inner.clone()
                    }
                });
                drop(engine);
                oracle.recover(&next, clock.clone(), &opts, &pending);
                io = Arc::new(HookIo::new(next));
                engine = Arc::new(open(io.clone(), clock.clone(), &opts));
                oracle.verify(&engine, &mut rng);
                for index in pending {
                    if oracle.batches[index].key.is_some() {
                        retry(&engine, &mut oracle, index);
                        retry(&engine, &mut oracle, index);
                    }
                }
            } else if rng.below(2) == 0 {
                engine.checkpoint_step_sim().unwrap();
            }
        }
    }
    oracle.history
}

#[test]
fn inline_group_publishes_once_and_replies_with_original_sequences() {
    let io = Arc::new(HookIo::sim(19));
    let pause = Arc::new(PublishPause::default());
    let opts = EngineOptions {
        tape: TapeOptions {
            inline_sim: true,
            publish_pause: Some(pause.clone()),
            ..TapeOptions::default()
        },
        fact_history: 1,
        ..EngineOptions::default()
    };
    let engine = open(io.clone(), Arc::new(SimClock::default()), &opts);
    let (scope, turn) = (one_scope(), NewTurn { role: Role::User, content: vec![1], meta: vec![] });
    engine.append(&scope, vec![turn.clone()], None).unwrap();
    let before = engine.metrics().unwrap();
    let syncs = io.tally(|op| op.is_sync() as u64);
    let first = engine.enqueue_sim(&scope, vec![turn.clone()], Some("same-group")).unwrap();
    let duplicate = engine.enqueue_sim(&scope, vec![turn.clone()], Some("same-group")).unwrap();
    let keyless = engine.enqueue_sim(&scope, vec![turn.clone()], None).unwrap();
    assert_eq!(engine.metrics().unwrap().durable_lsn, before.durable_lsn);
    let retried = std::thread::scope(|threads| {
        let (engine, scope, turn) = (&engine, &scope, &turn);
        let (arrived, release) = pause.arm();
        let publishing = threads.spawn(move || engine.step_sim().unwrap());
        arrived.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let parked = engine.metrics().unwrap();
        assert_eq!(parked.dedup_entries, before.dedup_entries + 1);
        assert_eq!(parked.durable_lsn, before.durable_lsn);
        assert!(engine.window(scope, WindowQuery::Since { seq: 2, limit: 1 }).unwrap().is_empty());

        let (started, starting) = mpsc::sync_channel(0);
        let (replied, reply) = mpsc::channel();
        let retrying = threads.spawn(move || {
            started.send(()).unwrap();
            replied.send(engine.append(scope, vec![turn.clone()], Some("same-group"))).unwrap();
        });
        starting.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(
            matches!(
                reply.recv_timeout(std::time::Duration::from_secs(1)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "retry returned before the tape watermark covered its receipt"
        );
        release.send(()).unwrap();
        publishing.join().unwrap();
        let retried = reply.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap();
        retrying.join().unwrap();
        let visible = engine.window(scope, WindowQuery::Since { seq: 2, limit: 1 }).unwrap();
        assert_eq!(visible.len(), 1, "an acknowledged concurrent retry must be readable");
        assert_eq!(visible[0].lsn, retried.last_lsn);
        assert_eq!(visible[0].content, turn.content);
        retried
    });
    let first = first.finish().unwrap();
    assert_eq!(first.seqs, vec![2]);
    assert!(!first.deduplicated, "the first queued request commits");
    // Writer-path cached replies: a retry parked behind publication, and one in the same group.
    assert_deduplicated(&first, &retried);
    assert_deduplicated(&first, &duplicate.finish().unwrap());
    assert_eq!(keyless.finish().unwrap().seqs, vec![3]);
    assert_eq!(syncs.load(Ordering::SeqCst), 1);
    let after = engine.metrics().unwrap();
    assert_eq!(after.writer_groups - before.writer_groups, 2);
    assert_eq!(after.writer_group_requests - before.writer_group_requests, 4);
    let (stages, earlier) = (after.writer_stages, before.writer_stages);
    assert!(stages.write_ns > earlier.write_ns, "the committed group's writes are timed");
    assert_eq!(stages.reply_ns, 0, "an inline core sends its own replies");
    assert_eq!(engine.window(&scope, WindowQuery::Last(10)).unwrap().len(), 3);

    // One inline group of fact writes, held between publication and the durable watermark.
    let fact = |key: &str, value: Option<u8>| NewFact {
        key: key.into(),
        content: value.map(|v| vec![v]).unwrap_or_default(),
        meta: vec![],
        agent: "a".into(),
    };
    let put = |key, value| engine.fact_put("t", fact(key, Some(value)), None, None).unwrap();
    put("k", 1);
    let (k, c, d) = (put("k", 2), put("c", 0), put("d", 0));
    let old = engine.fact_history("t", "k", usize::MAX).unwrap();
    assert!(old.truncated && old.revisions.len() == 1);
    let d_old = engine.fact_history("t", "d", usize::MAX).unwrap();
    assert!(!d_old.truncated && d_old.revisions.len() == 1);
    let queue = |key, value: Option<u8>, idem, if_lsn| {
        engine.enqueue_fact_sim("t", fact(key, value), value.is_none(), idem, if_lsn).unwrap()
    };
    let queued = [
        queue("k", Some(3), None, None),
        queue("c", Some(1), Some("c1"), Some(c.lsn)),
        queue("c", Some(2), None, Some(c.lsn)),
        queue("d", Some(1), None, None),
        queue("d", Some(2), None, None),
        queue("d", None, None, None),
        queue("c", Some(1), Some("c1"), Some(c.lsn)),
    ];
    std::thread::scope(|threads| {
        let (arrived, release) = pause.arm();
        let stepping = threads.spawn(|| engine.step_sim().unwrap());
        arrived.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        // Truncation drops `k`'s first revision while readers still see the old watermark.
        let parked = engine.fact_history("t", "k", usize::MAX).unwrap();
        assert_eq!(parked, old, "truncated holds at the old watermark");
        // Three `d` writes drop a revision of this group only. Nothing older exists.
        let parked = engine.fact_history("t", "d", usize::MAX).unwrap();
        assert_eq!(parked, d_old, "no truncation at the old watermark");
        for (key, lsn) in [("k", k.lsn), ("c", c.lsn), ("d", d.lsn)] {
            let value = engine.fact_get("t", key).unwrap().map(|fact| fact.lsn);
            assert_eq!(value, Some(lsn), "{key}: the pre-group value, never a group one or none");
        }
        release.send(()).unwrap();
        stepping.join().unwrap();
    });
    let [k3, c1, c2, d1, d2, d3, retried] = queued.map(SimFact::finish);
    let (k3, c1) = (k3.unwrap(), c1.unwrap());
    let error = c2.unwrap_err();
    assert!(matches!(error.kind, ErrorKind::Conflict), "the same if_lsn twice in one group");
    assert_eq!(error.outcome, Outcome::NotCommitted);
    let lsns = [d1, d2, d3].map(|written| written.unwrap().lsn);
    assert!(lsns.is_sorted(), "{lsns:?}");
    assert_eq!(engine.fact_get("t", "d").unwrap(), None, "put then delete in one group");
    let retried = retried.unwrap();
    assert!(retried.deduplicated && (retried.txn, retried.lsn) == (c1.txn, c1.lsn));
    assert_eq!(engine.fact_get("t", "c").unwrap().unwrap().lsn, c1.lsn);
    assert_eq!(engine.fact_get("t", "k").unwrap().unwrap().lsn, k3.lsn);
}

/// Studio pins a session forget to the `as_of_lsn` of the view the user saw. A turn published
/// but not yet durable must not be under that pin, or the forget would hide a turn no view
/// showed.
#[test]
fn window_as_of_lsn_never_covers_an_unseen_turn() {
    let pause = Arc::new(PublishPause::default());
    let tape = TapeOptions {
        inline_sim: true,
        publish_pause: Some(pause.clone()),
        ..TapeOptions::default()
    };
    let opts = EngineOptions { tape, ..EngineOptions::default() };
    let engine = open(Arc::new(HookIo::sim(41)), Arc::new(SimClock::default()), &opts);
    let scope = one_scope();
    let turn = |value| NewTurn { role: Role::User, content: vec![value], meta: vec![] };
    engine.append(&scope, vec![turn(1), turn(2)], None).unwrap();
    let third = engine.enqueue_sim(&scope, vec![turn(3)], None).unwrap();
    let pin = std::thread::scope(|threads| {
        let (arrived, release) = pause.arm();
        let stepping = threads.spawn(|| engine.step_sim().unwrap());
        arrived.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        // Parked after publish, before the durable watermark moves.
        let durable = engine.metrics().unwrap().durable_lsn;
        let window = engine.window_budget(&scope, WindowQuery::Last(50), usize::MAX).unwrap();
        let page = engine.sessions("t", SessionQuery { after: None, limit: 10 }).unwrap();
        assert_eq!((window.as_of_lsn, page.as_of_lsn), (durable, durable));
        assert_eq!(window.turns.iter().map(|t| t.seq).collect::<Vec<_>>(), [1, 2]);
        release.send(()).unwrap();
        stepping.join().unwrap();
        window.as_of_lsn
    });
    let third = third.finish().unwrap();
    assert!(third.first_lsn > pin, "the pin never reaches the unseen turn");
    let session = Selector::Session { agent: "a".into(), session: "s".into() };
    engine.forget("t", session, Some("studio"), Some(pin)).unwrap();
    let window = engine.window(&scope, WindowQuery::Last(50)).unwrap();
    let seen: Vec<_> = window.iter().map(|t| (t.seq, t.redacted, t.content.clone())).collect();
    assert_eq!(seen, [(1, true, vec![]), (2, true, vec![]), (3, false, vec![3])]);
}

fn inline_opts() -> EngineOptions {
    let tape = TapeOptions { segment_size: 64 * 1024, inline_sim: true, ..TapeOptions::default() };
    EngineOptions { tape, ..EngineOptions::default() }
}

fn one_scope() -> Scope {
    Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() }
}

#[test]
fn fact_groups_keep_retained_pointers_bounded() {
    let (io, clock) = (Arc::new(SimDisk::new(37)), Arc::new(SimClock::default()));
    let opts = EngineOptions { fact_history: 1, ..inline_opts() };
    let engine = open(io.clone(), clock.clone(), &opts);
    let fact = |key: String| NewFact { key, content: vec![1], meta: vec![], agent: "a".into() };
    engine.fact_put("t", fact("hot".into()), None, None).unwrap();
    let bounded = |engine: &Engine, per_key: usize| {
        let metrics = engine.metrics().unwrap();
        assert!(metrics.fact_revisions <= metrics.facts * per_key, "{metrics:?}");
    };
    // Two groups of ten writes to one key, then groups that write only distinct other keys.
    for group in 0..4 {
        let queued: Vec<_> = (0..10)
            .map(|i| {
                let key = if group < 2 { "hot".into() } else { format!("{group}-{i}") };
                engine.enqueue_fact_sim("t", fact(key), false, None, None).unwrap()
            })
            .collect();
        engine.step_sim().unwrap();
        queued.into_iter().for_each(|reply| assert!(reply.finish().is_ok()));
        bounded(&engine, opts.fact_history + 2);
    }
    drop(engine);
    bounded(&open(io, clock, &opts), opts.fact_history + 1);
}

#[test]
fn publish_during_checkpoint_keeps_the_new_page_dirty() {
    let (io, clock, opts) =
        (Arc::new(HookIo::sim(31)), Arc::new(SimClock::default()), inline_opts());
    let engine = Arc::new(open(io.clone(), clock.clone(), &opts));
    let scope = one_scope();
    let turn = |value| NewTurn { role: Role::User, content: vec![value], meta: vec![] };
    engine.append(&scope, vec![turn(1), turn(2)], None).unwrap();
    let fact =
        |value| NewFact { key: "k".into(), content: vec![value], meta: vec![], agent: "a".into() };
    let first = engine.fact_put("t", fact(1), None, None).unwrap();
    let queued = engine.enqueue_sim(&scope, vec![turn(3)], None).unwrap();
    // A write to a key the snapshot holds, published before the install.
    let later = engine.enqueue_fact_sim("t", fact(2), false, None, None).unwrap();
    let (weak, fired) = (Arc::downgrade(&engine), Arc::new(AtomicBool::new(false)));
    let completed = fired.clone();
    after_nth(&io, 2, move || {
        weak.upgrade().unwrap().step_sim().unwrap();
        completed.store(true, Ordering::SeqCst);
    });
    assert_eq!(engine.checkpoint().unwrap(), first.lsn);
    assert!(fired.load(Ordering::SeqCst));
    let receipt = queued.finish().unwrap();
    assert_eq!(receipt.seqs, vec![3]);
    let later = later.finish().unwrap();
    assert_eq!(engine.checkpoint().unwrap(), later.lsn);
    drop(engine);
    let engine = open(Arc::new(io.inner.crash().unwrap()), clock, &opts);
    let value = engine.fact_get("t", "k").unwrap().map(|fact| fact.lsn);
    assert_eq!(value, Some(later.lsn), "the key stayed dirty, so the next checkpoint wrote it");
    let window = engine.window(&scope, WindowQuery::Last(10)).unwrap();
    assert_eq!(window.iter().map(|t| t.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(
        window.iter().map(|t| t.content.clone()).collect::<Vec<_>>(),
        vec![vec![1], vec![2], vec![3]]
    );
    assert_eq!(engine.metrics().unwrap().replayed_records, 0);
}

#[test]
fn publish_during_checkpoint_keeps_new_forget_dirty() {
    let (io, clock, opts) =
        (Arc::new(HookIo::sim(32)), Arc::new(SimClock::default()), inline_opts());
    let engine = Arc::new(open(io.clone(), clock.clone(), &opts));
    let (a, b) = (one_scope(), Scope { session: "b".into(), ..one_scope() });
    let turn = |value| NewTurn { role: Role::User, content: vec![value], meta: vec![] };
    let fact = |key: &str, value| NewFact {
        key: key.into(),
        content: vec![value],
        meta: vec![],
        agent: "a".into(),
    };
    let seeded = engine.append(&a, vec![turn(1), turn(2), turn(3)], None).unwrap();
    let in_b = engine.append(&b, vec![turn(4), turn(5)], None).unwrap();
    let k1 = engine.fact_put("t", fact("k", 1), None, None).unwrap();
    engine.fact_put("t", fact("j", 1), None, None).unwrap();
    engine.checkpoint().unwrap();
    let session_a = Selector::Session { agent: "a".into(), session: "s".into() };
    let (x, y) = (in_b.first_lsn, in_b.last_lsn);
    engine.forget("t", session_a.clone(), None, Some(seeded.first_lsn + 1)).unwrap();
    engine.forget("t", Selector::FactKey("k".into()), None, Some(k1.lsn)).unwrap();
    engine.forget("t", Selector::Lsns(vec![x]), None, None).unwrap();
    let later = engine.append(&a, vec![turn(6), turn(7)], None).unwrap();
    let k2 = engine.fact_put("t", fact("k", 2), None, None).unwrap();
    // Published after the snapshot, before the dirty clear: 2 raised entries, 1 new key, a
    // held pair again and a new one. A conditional put sees the forget staged before it.
    let queued = [
        engine.enqueue_forget_sim("t", session_a, None, Some(later.first_lsn)).unwrap(),
        engine.enqueue_forget_sim("t", Selector::FactKey("j".into()), None, None).unwrap(),
        engine.enqueue_forget_sim("t", Selector::FactKey("k".into()), None, None).unwrap(),
        engine.enqueue_forget_sim("t", Selector::Lsns(vec![x, y]), None, None).unwrap(),
    ];
    let put = engine.enqueue_fact_sim("t", fact("j", 2), false, None, Some(0)).unwrap();
    let (weak, fired) = (Arc::downgrade(&engine), Arc::new(AtomicBool::new(false)));
    let completed = fired.clone();
    after_nth(&io, 2, move || {
        weak.upgrade().unwrap().step_sim().unwrap();
        completed.store(true, Ordering::SeqCst);
    });
    assert_eq!(engine.checkpoint().unwrap(), k2.lsn, "the boundary is below the queued forgets");
    assert!(fired.load(Ordering::SeqCst));
    let forgotten = queued.map(|reply| reply.finish().unwrap());
    // A staged forget hides j, so `if_lsn = 0` passes.
    let put = put.finish().unwrap();
    assert_eq!(forgotten[2].cutoff, forgotten[2].lsn);
    assert_eq!(engine.checkpoint().unwrap(), put.lsn, "the boundary is above them");
    type Seen = (Vec<Turn>, Vec<Turn>, Option<u64>, Option<u64>, Vec<String>, [usize; 3]);
    let seen = |engine: &Engine| -> Seen {
        let window = |scope| engine.window(scope, WindowQuery::Last(10)).unwrap();
        let lsn = |key| engine.fact_get("t", key).unwrap().map(|fact| fact.lsn);
        let query =
            RecordQuery { before: None, limit: 100, kind: None, max_bytes: 1 << 20, visits: 100 };
        let page = engine.records("t", query).unwrap();
        let records = page.records.iter().map(|record| match record {
            Record::Turn { turn, .. } => format!("{} {}", turn.lsn, turn.redacted),
            Record::Fact { revision, .. } => format!("{} {}", revision.lsn, revision.redacted),
        });
        let m = engine.metrics().unwrap();
        let counts = [m.forgotten_sessions, m.forgotten_fact_keys, m.forgotten_lsns];
        (window(&a), window(&b), lsn("k"), lsn("j"), records.collect(), counts)
    };
    let before = seen(&engine);
    let hidden = |turns: &[Turn]| turns.iter().map(|t| t.redacted).collect::<Vec<_>>();
    assert_eq!(hidden(&before.0), [true, true, true, true, false], "A up to its 4th turn");
    assert_eq!(hidden(&before.1), [true, true], "x and y");
    assert_eq!((before.2, before.3), (None, Some(put.lsn)), "k forgotten, j written again");
    assert_eq!(before.5, [1, 2, 2]);
    drop(engine);
    let image = io.inner.crash().unwrap();
    let engine = open(Arc::new(image.clone()), clock.clone(), &opts);
    assert_eq!(engine.metrics().unwrap().replayed_records, 0, "nothing came from replay");
    assert_eq!(seen(&engine), before, "every queued forget stayed dirty, so it was written");
    drop(engine);
    for path in image.list(Path::new("db")).unwrap() {
        let name = path.file_name().unwrap().to_str().unwrap();
        if name.starts_with("ckpt-") || name == "CHECKPOINT" {
            image.remove(&path).unwrap();
        }
    }
    let engine = open(Arc::new(image), clock, &opts);
    assert!(engine.metrics().unwrap().replayed_records > 0);
    assert_eq!(seen(&engine), before, "a full replay gives the same answers");
}

#[test]
fn legacy_checkpoint_magic_falls_back_to_full_replay() {
    let (io, clock, opts) =
        (Arc::new(SimDisk::new(33)), Arc::new(SimClock::default()), inline_opts());
    let (scope, turn) = (one_scope(), NewTurn { role: Role::User, content: vec![7], meta: vec![] });
    let engine = open(io.clone(), clock.clone(), &opts);
    engine.append(&scope, vec![turn.clone(); 2], Some("original")).unwrap();
    engine.checkpoint().unwrap();
    drop(engine);
    let path = io
        .list(Path::new("db"))
        .unwrap()
        .into_iter()
        .find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("ckpt-"))
        .unwrap();
    let file = io.open(&path).unwrap();
    let mut header = [0; 28];
    assert_eq!(io.read_at(file, 0, &mut header).unwrap(), header.len());
    header[8..12].copy_from_slice(b"ZCK1");
    let crc = atticdb_tape::sim::crc32c(&header[4..]);
    header[..4].copy_from_slice(&crc.to_le_bytes());
    io.write_at(file, 0, &header).unwrap();
    io.sync_data(file).unwrap();
    io.close(file).unwrap();
    let engine = open(io, clock, &opts);
    assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 1);
    assert_eq!(engine.metrics().unwrap().replayed_records, 3);
    assert_eq!(engine.window(&scope, WindowQuery::Last(10)).unwrap().len(), 2);
    assert_eq!(engine.append(&scope, vec![turn; 2], Some("original")).unwrap().seqs, vec![1, 2]);
}

#[test]
fn missing_checkpoint_filename_after_crash_replays_the_tape() {
    let (opts, scope) = (inline_opts(), one_scope());
    let turn = NewTurn { role: Role::User, content: vec![9], meta: vec![] };
    let mut observed = false;
    for seed in 0..128 {
        eprintln!("case: seed={seed}");
        let io = Arc::new(HookIo::sim(seed));
        let clock = Arc::new(SimClock::default());
        let engine = open(io.clone(), clock.clone(), &opts);
        let receipt = engine.append(&scope, vec![turn.clone(); 2], None).unwrap();
        // Capture after manifest rename, before the sole checkpoint directory sync.
        let take = cut_at(&io, 7);
        engine.checkpoint().unwrap();
        let image = take().unwrap();
        drop(engine);
        if image.exists(Path::new("db/CHECKPOINT")).unwrap()
            && !image
                .exists(&Path::new("db").join(format!("ckpt-{}.zdb", receipt.last_lsn)))
                .unwrap()
        {
            let engine = Engine::open_with(Arc::new(image), clock, "db", opts.clone()).unwrap();
            assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 1, "seed={seed}");
            assert_eq!(engine.metrics().unwrap().replayed_records, 3, "seed={seed}");
            assert_eq!(
                engine.window(&scope, WindowQuery::Last(10)).unwrap().len(),
                2,
                "seed={seed}"
            );
            observed = true;
            break;
        }
    }
    assert!(observed, "seeds=0..128: no missing-reference crash image was exercised");
}

#[test]
fn seeded_engine_recovery() {
    let first = std::env::var("ATTICDB_SIM_SEED").map_or(0, |v| v.parse::<u64>().unwrap());
    let count = std::env::var("ATTICDB_SIM_SEEDS").map_or_else(
        |_| if std::env::var_os("ATTICDB_SIM_SEED").is_some() { 1 } else { 500 },
        |v| v.parse::<u64>().unwrap(),
    );
    assert!(count > 0, "ATTICDB_SIM_SEEDS must be positive");
    for offset in 0..count {
        let seed = first.wrapping_add(offset);
        if let Err(error) = std::panic::catch_unwind(|| run_seed(seed)) {
            eprintln!("engine simulation seed={seed}\nrepro: ATTICDB_SIM_SEED={seed} cargo test -p atticdb-core --release --features sim --test engine_sim");
            std::panic::resume_unwind(error);
        }
    }
    eprintln!("engine simulation passed: {count} seeds starting at {first}");
}
