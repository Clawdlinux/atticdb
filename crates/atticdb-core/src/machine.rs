use crate::checkpoint::{read_page, Command};
use crate::dedup::{digest, hash_key, Dedup, DedupKey, Entry, Scope};
use crate::error::{lock, Result};
use crate::fact;
use crate::facts::{current, FactKey, Rev};
use crate::forget::Selector;
use crate::forgets::{cutoff, unique};
use crate::session::{address, Key};
use crate::state::State;
use crate::tenant::Registry;
use crate::turn::{decode_ref as decode, set_seq};
use crate::{EngineError, EngineOptions, ErrorKind};
use atticdb_tape::{
    Committed, CommittedRecord, Error, Io, Kind, Outcome, Prepare, PreparedBatch, StateMachine,
    WriteError,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Trailing byte on a cached reply's 8-byte sequence. Only replies carry it. Dedup
/// entries and checkpoints keep the plain 8-byte form.
pub(crate) const REPLY_DEDUPLICATED: u8 = 1;

pub(crate) struct Machine {
    state: Arc<Mutex<State>>,
    io: Arc<dyn Io>,
    dir: PathBuf,
    opts: EngineOptions,
    tenants: Registry,
    dedup: Dedup,
    facts: HashMap<FactKey, Rev>,
    /// Fact-key forgets staged in the open group: (highest cutoff, newest forget LSN).
    forget_keys: HashMap<FactKey, (u64, u64)>,
    /// LSN pairs staged in the open group and not yet published, to their forget's LSN.
    forget_lsns: HashMap<(u32, u64), u64>,
    /// Staged forgets' LSNs and pairs, oldest first, so publication drops them in order.
    forget_order: VecDeque<(u64, Vec<(u32, u64)>)>,
    lsn: u64,
    txn: u64,
    digest_txn: u64,
    first_lsn: u64,
    first_seq: u64,
    hash: blake3::Hasher,
    #[cfg(test)]
    prepare_hash_bytes: usize,
}
impl Machine {
    pub(crate) fn new(
        state: Arc<Mutex<State>>,
        io: Arc<dyn Io>,
        dir: PathBuf,
        opts: EngineOptions,
    ) -> Result<Self> {
        let s = lock(&state)?;
        let dedup = Dedup::new(opts.dedup_per_tenant, opts.dedup_global);
        let machine = Self {
            state: state.clone(),
            io,
            dir,
            opts,
            tenants: s.tenants.clone(),
            dedup,
            facts: HashMap::new(),
            forget_keys: HashMap::new(),
            forget_lsns: HashMap::new(),
            forget_order: VecDeque::new(),
            lsn: s.published_lsn,
            txn: s.txn,
            digest_txn: 0,
            first_lsn: 0,
            first_seq: 0,
            hash: blake3::Hasher::new(),
            #[cfg(test)]
            prepare_hash_bytes: 0,
        };
        Ok(machine)
    }
    fn prepare_inner(&mut self, batch: &mut PreparedBatch) -> Result<Prepare> {
        let invalid = || EngineError::rejected(ErrorKind::InvalidInput);
        let records: Vec<_> = batch.records().collect();
        let count = records.len();
        let create = records.first().filter(|r| r.kind == Kind::TenantCreate);
        let body = &records[usize::from(create.is_some())..];
        let head = body.first().ok_or_else(invalid)?;
        let tenant = if let Some(record) = create {
            let name = std::str::from_utf8(&record.content).map_err(|_| invalid())?;
            if name.len() > 255 || self.tenants.get(name).is_some() {
                return Err(invalid());
            }
            if self.tenants.entries.len() >= self.opts.max_tenants.min(10_000) {
                return Err(EngineError::rejected(ErrorKind::ResourceExhausted("tenants")));
            }
            self.tenants.entries.len() as u32 + 1
        } else {
            head.tenant_id
        };
        if tenant == 0 || (create.is_none() && tenant as usize > self.tenants.entries.len()) {
            return Err(invalid());
        }
        // A fact or forget write is exactly one record. Turns, facts and forgets never share a
        // batch.
        let mut forget = None;
        let (session, fact, scope, idem) = if head.kind == Kind::Forget {
            let decoded = crate::forget::decode(&head.content)?;
            let empty = match &decoded.selector {
                Selector::Lsns(lsns) => lsns.is_empty() || lsns.contains(&0),
                Selector::Session { .. } | Selector::FactKey(_) => false,
            };
            if body.len() != 1 || empty || decoded.up_to_lsn == Some(0) {
                return Err(invalid());
            }
            let idem = decoded.idem_key.clone();
            forget = Some(decoded);
            (None, None, Scope::Forget, idem)
        } else if head.kind == Kind::Fact {
            let fact = fact::decode(&head.content)?;
            if body.len() != 1
                || fact.key.is_empty()
                || !fact.source_refs.is_empty()
                || (fact.tombstone && !(fact.content.is_empty() && fact.meta.is_empty()))
            {
                return Err(invalid());
            }
            let staged = Some(((tenant, fact.key.clone()), fact.tombstone));
            (None, staged, Scope::Fact(fact.key), fact.idem_key)
        } else {
            let first = decode(&head.content)?;
            let key = Key(tenant, first.agent_id.to_owned(), first.session_id.to_owned());
            for (i, record) in body.iter().enumerate() {
                let next;
                let decoded = if i == 0 {
                    &first
                } else {
                    next = decode(&record.content)?;
                    &next
                };
                if record.kind != Kind::Turn
                    || decoded.seq != 0
                    || decoded.agent_id != key.1
                    || decoded.session_id != key.2
                    || decoded.idem_key != first.idem_key
                    || (create.is_none() && record.tenant_id != tenant)
                {
                    return Err(invalid());
                }
            }
            let scope = Scope::Session(key.2.clone());
            (Some((key, body.len())), None, scope, first.idem_key.map(str::to_owned))
        };
        let payloads = || body.iter().map(|record| record.content.as_slice());
        let digest = match batch.application_digest() {
            Some(digest) => digest,
            None if idem.is_some() => {
                #[cfg(test)]
                {
                    self.prepare_hash_bytes += payloads().map(<[u8]>::len).sum::<usize>();
                }
                // Over version 1 bytes, as the engine digests before it compresses.
                let canonical = payloads()
                    .map(|payload| match session {
                        Some(_) => crate::turn::canonical(payload),
                        None => Ok(std::borrow::Cow::Borrowed(payload)),
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                digest(canonical.iter().map(|payload| payload.as_ref()))
            }
            None => [0; 32],
        };
        // Hashed once. The staged entry keeps the hash for publication and eviction.
        let dedup_key = idem.map(|id| DedupKey(tenant, scope, id)).map(|key| (hash_key(&key), key));
        let mut state = lock(&self.state)?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if let Some(entry) = dedup_key.as_ref().and_then(|(hash, key)| {
            self.dedup.get_hashed(*hash, key).or_else(|| state.dedup.get_hashed(*hash, key))
        }) {
            let Committed::Durable { txn, first_lsn, last_lsn, mut reply } =
                entry.resolve(digest)?;
            reply.push(REPLY_DEDUPLICATED);
            return Ok(Prepare::Reply(Committed::Durable { txn, first_lsn, last_lsn, reply }));
        }
        if dedup_key.is_some() && !self.dedup.can_stage(tenant) {
            return Err(EngineError::rejected(ErrorKind::ResourceExhausted("prepared_dedup")));
        }
        if let Some((key, _)) = &fact {
            let newest = self
                .facts
                .get(key)
                .or_else(|| state.facts.map.get(key).and_then(|r| r.list.last()));
            // A hidden newest revision is no current value, so `if_lsn = 0` passes (I9).
            let hidden = newest.is_some_and(|rev| {
                state.forgets.hides_fact(key, rev.lsn) || self.stages_hidden(key, rev.lsn)
            });
            let current = if hidden { 0 } else { current(newest) };
            if batch.application_arg().is_some_and(|lsn| lsn != current) {
                return Err(EngineError::rejected(ErrorKind::Conflict));
            }
            if newest.is_none() && state.facts.full(&self.facts) {
                return Err(EngineError::rejected(ErrorKind::ResourceExhausted("facts")));
            }
        }
        // Pairs this forget may add, counted like `Facts::full`: held and staged pairs once.
        let pairs = match forget.as_ref().map(|forget| &forget.selector) {
            Some(Selector::Lsns(lsns)) => unique(lsns)
                .into_iter()
                .map(|lsn| (tenant, lsn))
                .filter(|pair| {
                    !state.forgets.lsns.contains(pair) && !self.forget_lsns.contains_key(pair)
                })
                .collect(),
            _ => Vec::new(),
        };
        if !pairs.is_empty()
            && state.forgets.lsns.len() + self.forget_lsns.len() + pairs.len()
                > state.forgets.max_lsns()
        {
            return Err(EngineError::rejected(ErrorKind::ResourceExhausted("forgets")));
        }
        let first_lsn = self
            .lsn
            .checked_add(1)
            .ok_or_else(|| EngineError::rejected(ErrorKind::ResourceExhausted("lsn")))?;
        let last_lsn = self
            .lsn
            .checked_add(count as u64)
            .ok_or_else(|| EngineError::rejected(ErrorKind::ResourceExhausted("lsn")))?;
        let txn = self
            .txn
            .checked_add(1)
            .ok_or_else(|| EngineError::rejected(ErrorKind::ResourceExhausted("txn")))?;
        let seq = match &session {
            Some((key, turns)) => Some(state.index.reserve(key, *turns)?),
            None => None,
        };
        if let Some(record) = create {
            let name = std::str::from_utf8(&record.content).map_err(|_| invalid())?;
            self.tenants.insert(name, tenant, first_lsn, self.opts.max_tenants)?;
        }
        let offset = usize::from(create.is_some());
        for (index, record) in batch.records_mut().enumerate() {
            record.tenant_id = tenant;
            if let Some(seq) = seq.filter(|_| index >= offset) {
                set_seq(&mut record.content, seq + (index - offset) as u64)?;
            }
        }
        self.lsn = last_lsn;
        self.txn = txn;
        let reply = seq.map_or_else(Vec::new, |seq| seq.to_le_bytes().to_vec());
        batch.set_reply(reply.clone());
        if let Some((hash, key)) = dedup_key {
            self.dedup.insert(Entry {
                hash,
                key,
                digest,
                committed: Committed::Durable { txn, first_lsn, last_lsn, reply },
            });
        }
        if let Some((key, deleted)) = fact {
            self.facts.insert(key, Rev { lsn: last_lsn, deleted });
        }
        if let Some(forget) = forget {
            let cut = cutoff(forget.up_to_lsn, last_lsn);
            if let Selector::FactKey(key) = forget.selector {
                let staged = self.forget_keys.entry((tenant, key)).or_insert((0, 0));
                *staged = (staged.0.max(cut), last_lsn);
            }
            let pairs: Vec<_> = pairs.into_iter().filter(|&(_, lsn)| lsn <= cut).collect();
            if !pairs.is_empty() {
                for pair in &pairs {
                    self.forget_lsns.insert(*pair, last_lsn);
                }
                self.forget_order.push_back((last_lsn, pairs));
            }
        }
        Ok(Prepare::Write)
    }

    /// True when a forget staged in the open group hides this fact revision.
    fn stages_hidden(&self, key: &FactKey, lsn: u64) -> bool {
        self.forget_keys.get(key).is_some_and(|&(cut, _)| lsn <= cut)
            || self.forget_lsns.contains_key(&(key.0, lsn))
    }

    /// Applies a published forget record to the forget set.
    fn install_forget(
        &mut self,
        state: &mut State,
        record: &CommittedRecord,
        replay: bool,
    ) -> Result<()> {
        let corrupt = || EngineError::rejected(ErrorKind::CorruptIndex);
        let forget = crate::forget::decode(&record.payload).map_err(|_| corrupt())?;
        let tenant = record.tenant_id;
        let cut = cutoff(forget.up_to_lsn, record.lsn);
        match &forget.selector {
            // Only sessions with a published turn and keys in the fact index are stored, so
            // both stay bounded by `max_sessions` and `max_facts` (I12).
            Selector::Session { agent, session } => {
                let key = Key(tenant, agent.clone(), session.clone());
                if state.index.sessions.get(&key).is_some_and(|entry| entry.len > 0) {
                    state.forgets.raise_session(key, cut);
                }
            }
            Selector::FactKey(key) => {
                let key = (tenant, key.clone());
                if state.facts.map.contains_key(&key) {
                    state.forgets.raise_key(key, cut);
                }
            }
            Selector::Lsns(lsns) => {
                if lsns.is_empty() || lsns.contains(&0) {
                    return Err(corrupt());
                }
                for lsn in unique(lsns).into_iter().filter(|&lsn| lsn <= cut) {
                    state.forgets.add_lsn((tenant, lsn))?;
                }
            }
        }
        if let Some(id) = forget.idem_key.filter(|_| replay) {
            let key = DedupKey(tenant, Scope::Forget, id);
            self.rebuild(state, record, key, &record.payload, Vec::new());
        }
        // Published now, so the staged overlay of this and older forgets can go.
        if self.forget_keys.values().any(|&(_, lsn)| lsn <= record.lsn) {
            self.forget_keys.retain(|_, &mut (_, lsn)| lsn > record.lsn);
        }
        while let Some((_, pairs)) = self.forget_order.front().filter(|(lsn, _)| *lsn <= record.lsn)
        {
            for pair in pairs {
                if self.forget_lsns.get(pair).is_some_and(|&lsn| lsn <= record.lsn) {
                    self.forget_lsns.remove(pair);
                }
            }
            self.forget_order.pop_front();
        }
        Ok(())
    }

    fn install(
        &mut self,
        state: &mut State,
        record: &CommittedRecord,
        floor: u64,
        replay: bool,
    ) -> Result<()> {
        let corrupt = || EngineError::rejected(ErrorKind::CorruptIndex);
        // Turns count uncompressed, so checkpoints bound replay work, not stored bytes.
        let mut logical = record.payload.len();
        if self.digest_txn != record.txn {
            self.digest_txn = record.txn;
            self.first_lsn = record.lsn;
            self.first_seq = 0;
            self.hash.reset();
        }
        if record.kind != Kind::TenantCreate
            && (record.tenant_id == 0 || record.tenant_id as usize > state.tenants.entries.len())
        {
            return Err(corrupt());
        }
        match record.kind {
            Kind::TenantCreate => {
                let name = std::str::from_utf8(&record.payload).map_err(|_| corrupt())?;
                state.tenants.insert(name, record.tenant_id, record.lsn, self.opts.max_tenants)?;
                if replay {
                    self.tenants.insert(
                        name,
                        record.tenant_id,
                        record.lsn,
                        self.opts.max_tenants,
                    )?;
                }
            }
            Kind::Turn => {
                let turn = decode(&record.payload).map_err(|_| corrupt())?;
                logical = turn.logical_len(record.payload.len());
                if self.first_seq == 0 {
                    self.first_seq = turn.seq;
                }
                let key =
                    Key(record.tenant_id, turn.agent_id.to_owned(), turn.session_id.to_owned());
                if replay {
                    if let Some(entry) = state.index.sessions.get(&key) {
                        if entry.len > 0 {
                            state.index.load(&key, address(entry.len).0, &mut |backing| {
                                read_page(self.io.as_ref(), &self.dir, backing)
                            })?;
                        }
                    }
                    state.index.reserve(&key, 1)?;
                }
                state.index.publish(&key, turn.seq, record.lsn)?;
                state.dirty.insert((key, address(turn.seq).0));
                if let Some(id) = turn.idem_key.filter(|_| replay) {
                    // Digests cover version 1 bytes, as the engine hashed them before compressing.
                    let canonical =
                        crate::turn::canonical(&record.payload).map_err(|_| corrupt())?;
                    let mut normalized = canonical.into_owned();
                    set_seq(&mut normalized, 0)?;
                    let key = DedupKey(
                        record.tenant_id,
                        Scope::Session(turn.session_id.to_owned()),
                        id.to_owned(),
                    );
                    let reply = self.first_seq.to_le_bytes().to_vec();
                    self.rebuild(state, record, key, &normalized, reply);
                }
            }
            Kind::Fact => {
                let (key, idem_key, deleted) =
                    fact::header(&record.payload).map_err(|_| corrupt())?;
                let key = (record.tenant_id, key);
                state.facts.install(&key, Rev { lsn: record.lsn, deleted }, floor)?;
                if self.facts.get(&key).is_some_and(|rev| rev.lsn == record.lsn) {
                    self.facts.remove(&key);
                }
                if let Some(id) = idem_key.filter(|_| replay) {
                    let key = DedupKey(record.tenant_id, Scope::Fact(key.1), id);
                    self.rebuild(state, record, key, &record.payload, Vec::new());
                }
            }
            Kind::Forget => self.install_forget(state, record, replay)?,
            _ => return Err(corrupt()),
        }
        // Move this transaction's staged retry entry once its last record is published.
        if let Some(entry) = self.dedup.entries.values().next().filter(|entry| {
            matches!(entry.committed, Committed::Durable { last_lsn, .. } if last_lsn == record.lsn)
        }) {
            let age = entry.age();
            if let Some(entry) = self.dedup.remove(age) {
                state.dedup.insert_arc(entry);
            }
        }
        state.bytes = state
            .bytes
            .saturating_add(logical as u64 + 40 + if state.txn != record.txn { 72 } else { 0 });
        state.txn = record.txn;
        state.published_lsn = record.lsn;
        if replay {
            self.lsn = record.lsn;
            self.txn = record.txn;
            state.replayed += 1;
        }
        Ok(())
    }

    /// Rebuilds a replayed retry entry. The digest covers the transaction's payloads so far.
    fn rebuild(
        &mut self,
        state: &mut State,
        record: &CommittedRecord,
        key: DedupKey,
        payload: &[u8],
        reply: Vec<u8>,
    ) {
        self.hash.update(&(payload.len() as u32).to_le_bytes());
        self.hash.update(payload);
        state.dedup.insert(Entry::new(
            key,
            *self.hash.finalize().as_bytes(),
            Committed::Durable {
                txn: record.txn,
                first_lsn: self.first_lsn,
                last_lsn: record.lsn,
                reply,
            },
        ));
    }
}
fn tape_error(error: EngineError) -> Error {
    match error.kind {
        ErrorKind::IdempotencyConflict => Error::Rejected("idempotency"),
        ErrorKind::ResourceExhausted(resource) => Error::Rejected(resource),
        ErrorKind::InvalidInput => Error::Rejected("invalid_input"),
        ErrorKind::Conflict => Error::Rejected("conflict"),
        ErrorKind::CorruptIndex => Error::Rejected("corrupt_index"),
        ErrorKind::Poisoned => Error::Poisoned,
        ErrorKind::Storage(error) => error,
    }
}
impl StateMachine for Machine {
    fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare {
        self.prepare_inner(batch).unwrap_or_else(|error| {
            Prepare::Reject(WriteError { kind: tape_error(error), outcome: Outcome::NotCommitted })
        })
    }
    fn publish(&mut self, records: &[CommittedRecord]) {
        let shared = self.state.clone();
        let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
        let first = records.first().map_or(0, |record| record.lsn);
        for record in records {
            if let Err(mut error) = self.install(&mut state, record, first, false) {
                error.outcome = Outcome::Committed;
                state.fail(error);
                break;
            }
        }
        if state.bytes.saturating_sub(state.checkpoint_bytes) >= self.opts.checkpoint_bytes
            && !state.checkpoint_pending.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            if let Some(sender) = &state.notify {
                let _ = sender.try_send(Command::Wake);
            }
        }
    }
    fn replay(&mut self, record: &CommittedRecord) -> atticdb_tape::Result<()> {
        let result = (|| {
            let shared = self.state.clone();
            let spill = {
                let state = lock(&shared).map_err(tape_error)?;
                state.published_lsn > state.watermark
                    && state.txn != record.txn
                    && state.index.dirty_bytes > self.opts.hot_page_bytes.saturating_sub(64 * 1024)
            };
            if spill {
                #[cfg(feature = "sim")]
                if self.opts.tape.inline_sim {
                    crate::checkpoint::save(self.io.as_ref(), &self.dir, &shared)
                        .map_err(tape_error)?;
                } else {
                    recovery_checkpoint(self.io.as_ref(), &self.dir, &shared)?;
                }
                #[cfg(not(feature = "sim"))]
                recovery_checkpoint(self.io.as_ref(), &self.dir, &shared)?;
            }
            let mut state = lock(&shared).map_err(tape_error)?;
            self.install(&mut state, record, u64::MAX, true).map_err(tape_error)
        })();
        if result.is_err() {
            if let Ok(mut state) = self.state.lock() {
                state.replay_failed = true;
            }
        }
        result
    }
}

fn recovery_checkpoint(
    io: &dyn Io,
    dir: &std::path::Path,
    shared: &Arc<Mutex<State>>,
) -> atticdb_tape::Result<()> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("atticdb-checkpoint-recovery".into())
            .spawn_scoped(scope, || crate::checkpoint::save(io, dir, shared))
            .map_err(Error::from)?
            .join()
            .map_err(|_| Error::Poisoned)?
            .map_err(tape_error)
    })
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{NewTurn, Role};
    use atticdb_tape::sim::{SimClock, WriteRequest, WriterCore};
    use atticdb_tape::{Durability, Kind, RecordInput, SimDisk};

    #[test]
    fn forget_and_mixed_fact_batches_reserve_nothing() {
        let turn = crate::turn::encode(
            "a",
            "s",
            0,
            &NewTurn { role: Role::User, content: vec![], meta: vec![] },
            Some("k"),
        )
        .unwrap();
        let fact = crate::fact::encode(&crate::fact::Fact {
            key: "k".into(),
            content: vec![1],
            meta: vec![],
            agent: "a".into(),
            source_refs: vec![],
            idem_key: Some("k".into()),
            tombstone: false,
        })
        .unwrap();
        let forget = crate::forget::encode(&crate::forget::Forget {
            selector: Selector::Lsns(vec![1]),
            idem_key: Some("k".into()),
            up_to_lsn: None,
        })
        .unwrap();
        for (case, records, prepares) in [
            ("lone forget", vec![(Kind::Forget, &forget)], true),
            ("forget then turn", vec![(Kind::Forget, &forget), (Kind::Turn, &turn)], false),
            ("turn then forget", vec![(Kind::Turn, &turn), (Kind::Forget, &forget)], false),
            ("fact then forget", vec![(Kind::Fact, &fact), (Kind::Forget, &forget)], false),
            ("forget then fact", vec![(Kind::Forget, &forget), (Kind::Fact, &fact)], false),
            ("turn payload as forget", vec![(Kind::Forget, &turn)], false),
            ("fact then turn", vec![(Kind::Fact, &fact), (Kind::Turn, &turn)], false),
            ("turn then fact", vec![(Kind::Turn, &turn), (Kind::Fact, &fact)], false),
            ("one key twice", vec![(Kind::Fact, &fact), (Kind::Fact, &fact)], false),
        ] {
            let opts = EngineOptions { max_forgotten_lsns: 2, ..EngineOptions::default() };
            let mut initial = State::new(&opts);
            initial.tenants.insert("t", 1, 1, 10_000).unwrap();
            let mut machine = Machine::new(
                Arc::new(Mutex::new(initial)),
                Arc::new(SimDisk::new(0)),
                "db".into(),
                opts,
            )
            .unwrap();
            let batch = |records: Vec<(Kind, &Vec<u8>)>| {
                let records = records
                    .into_iter()
                    .map(|(kind, content)| RecordInput {
                        kind,
                        tenant_id: 1,
                        content: content.clone(),
                    })
                    .collect();
                PreparedBatch::new(records, Durability::Durable).unwrap()
            };
            match machine.prepare(&mut batch(records)) {
                Prepare::Write if prepares => {
                    assert_eq!(machine.lsn, 1, "{case}");
                    assert_eq!(machine.forget_lsns.len(), 1, "{case}: LSN 1 is at the cutoff");
                    assert_eq!(machine.dedup.entries.len(), 1, "{case}");
                    // The cap counts pairs staged in the open group, and each pair once.
                    for (lsns, written) in [(vec![1, 2, 3], false), (vec![1, 2, 2], true)] {
                        let forget = crate::forget::encode(&crate::forget::Forget {
                            selector: Selector::Lsns(lsns.clone()),
                            idem_key: None,
                            up_to_lsn: None,
                        })
                        .unwrap();
                        let prepared = machine.prepare(&mut batch(vec![(Kind::Forget, &forget)]));
                        match prepared {
                            Prepare::Write => assert!(written, "{case}: {lsns:?}"),
                            Prepare::Reject(error) => {
                                let kind = EngineError::write(error).kind;
                                assert!(!written, "{case}: {lsns:?}");
                                assert!(matches!(kind, ErrorKind::ResourceExhausted("forgets")));
                            }
                            Prepare::Reply(_) => panic!("{case}: no retry key"),
                        }
                    }
                    assert_eq!(machine.forget_lsns.len(), 2, "{case}: staged across batches");
                    continue;
                }
                Prepare::Reject(error) if !prepares => {
                    let kind = EngineError::write(error).kind;
                    assert!(matches!(kind, ErrorKind::InvalidInput), "{case}");
                }
                _ => panic!("{case}: unexpected prepare result"),
            }
            assert_eq!(machine.lsn, 0, "{case}");
            assert!(machine.facts.is_empty() && machine.dedup.entries.is_empty(), "{case}");
            assert!(machine.forget_lsns.is_empty() && machine.forget_keys.is_empty(), "{case}");
        }
    }

    #[test]
    fn over_threshold_publications_queue_only_one_wake() {
        let opts = EngineOptions { checkpoint_bytes: 1, ..EngineOptions::default() };
        let shared = Arc::new(Mutex::new(State::new(&opts)));
        let (sender, receiver) = std::sync::mpsc::sync_channel(16);
        {
            let mut state = shared.lock().unwrap();
            state.notify = Some(sender);
            state.bytes = 100;
        }

        let mut machine =
            Machine::new(shared, Arc::new(SimDisk::new(0)), "db".into(), opts).unwrap();
        for _ in 0..100 {
            machine.publish(&[]);
        }
        assert_eq!(receiver.try_iter().count(), 1);
    }

    #[test]
    fn keyless_prepare_does_not_hash_a_retry_digest() {
        for idem in [None, Some("key")] {
            let opts = EngineOptions::default();
            let mut initial = State::new(&opts);
            initial.tenants.insert("t", 1, 1, 10_000).unwrap();
            let mut machine = Machine::new(
                Arc::new(Mutex::new(initial)),
                Arc::new(SimDisk::new(18)),
                "db".into(),
                opts,
            )
            .unwrap();
            let content = crate::turn::encode(
                "a",
                "s",
                0,
                &NewTurn { role: Role::User, content: vec![1; 8192], meta: vec![] },
                idem,
            )
            .unwrap();
            let mut batch = PreparedBatch::new(
                vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content }],
                Durability::Durable,
            )
            .unwrap();
            assert!(matches!(machine.prepare(&mut batch), Prepare::Write));
            assert_eq!(machine.prepare_hash_bytes > 0, idem.is_some());
        }
    }

    #[test]
    fn prepare_rejections_keep_typed_engine_errors() {
        for corrupt in [false, true] {
            let disk = Arc::new(SimDisk::new(9));
            let opts = EngineOptions::default();
            let mut initial = State::new(&opts);
            initial.tenants.insert("t", 1, 1, 10_000).unwrap();
            let content = if corrupt {
                initial.fail(EngineError::rejected(ErrorKind::CorruptIndex));
                crate::turn::encode(
                    "a",
                    "s",
                    0,
                    &NewTurn { role: Role::User, content: vec![1], meta: vec![] },
                    None,
                )
                .unwrap()
            } else {
                vec![99]
            };
            let mut machine =
                Machine::new(Arc::new(Mutex::new(initial)), disk, "db".into(), opts).unwrap();
            let mut batch = PreparedBatch::new(
                vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content }],
                Durability::Durable,
            )
            .unwrap();
            let Prepare::Reject(error) = machine.prepare(&mut batch) else {
                panic!("expected rejection");
            };
            let error = EngineError::write(error);
            assert_eq!(error.outcome, Outcome::NotCommitted);
            if corrupt {
                assert!(matches!(error.kind, ErrorKind::CorruptIndex));
            } else {
                assert!(matches!(error.kind, ErrorKind::InvalidInput));
            }
        }
        let error = EngineError::write(WriteError {
            kind: Error::Poisoned,
            outcome: Outcome::NotCommitted,
        });
        assert_eq!(format!("{:?}", error.kind), "Poisoned");
    }

    #[test]
    fn prepared_overlay_preserves_published_entries_until_publish() {
        let disk = Arc::new(SimDisk::new(8));
        let opts =
            EngineOptions { dedup_per_tenant: 2, dedup_global: 2, ..EngineOptions::default() };
        let mut initial = State::new(&opts);
        initial.tenants.insert("t", 1, 1, 10_000).unwrap();
        initial.published_lsn = 3;
        initial.txn = 1;
        for age in 2..=3 {
            initial.dedup.insert(Entry::new(
                DedupKey(1, Scope::Session(age.to_string()), "old".into()),
                [0; 32],
                Committed::Durable {
                    txn: 1,
                    first_lsn: age,
                    last_lsn: age,
                    reply: 1u64.to_le_bytes().to_vec(),
                },
            ));
        }
        let shared = Arc::new(Mutex::new(initial));
        let mut machine =
            Machine::new(shared.clone(), disk.clone(), "db".into(), opts.clone()).unwrap();
        assert!(machine.dedup.entries.is_empty());
        let mut records = Vec::new();
        for (i, session) in ["new1", "new2"].into_iter().enumerate() {
            let content = crate::turn::encode(
                "a",
                session,
                0,
                &NewTurn { role: Role::User, content: vec![1], meta: vec![] },
                Some("key"),
            )
            .unwrap();
            let mut batch = PreparedBatch::new(
                vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content }],
                Durability::Durable,
            )
            .unwrap();
            assert!(matches!(machine.prepare(&mut batch), Prepare::Write));
            assert_eq!(shared.lock().unwrap().dedup.entries.len(), 2);
            assert_eq!(machine.dedup.entries.len(), i + 1);
            records.push(CommittedRecord {
                lsn: 4 + i as u64,
                txn: 2 + i as u64,
                kind: Kind::Turn,
                tenant_id: 1,
                commit_time_ns: 0,
                payload: Arc::from(batch.records().next().unwrap().content.as_slice()),
            });
        }
        assert_eq!(
            crate::checkpoint::save(disk.as_ref(), std::path::Path::new("db"), &shared).unwrap(),
            3
        );
        let checkpoint = crate::checkpoint::load(disk.as_ref(), std::path::Path::new("db"), &opts);
        for age in 2..=3 {
            assert!(checkpoint
                .dedup
                .get(&DedupKey(1, Scope::Session(age.to_string()), "old".into()))
                .is_some());
        }
        assert!(checkpoint
            .dedup
            .get(&DedupKey(1, Scope::Session("new1".into()), "key".into()))
            .is_none());
        machine.publish(&records);
        assert!(machine.dedup.entries.is_empty());
        assert_eq!(shared.lock().unwrap().dedup.entries.len(), 2);
    }

    #[test]
    fn flush_cleans_published_pages_and_hot_page_retry_works_during_preparation() {
        let disk = Arc::new(SimDisk::new(10));
        let opts = EngineOptions { hot_page_bytes: 256, ..EngineOptions::default() };
        let mut initial = State::new(&opts);
        initial.tenants.insert("t", 1, 1, 10_000).unwrap();
        let published = Key(1, "a".into(), "published".into());
        initial.index.reserve(&published, 1).unwrap();
        initial.index.publish(&published, 1, 2).unwrap();
        initial.dirty.insert((published.clone(), 0));
        initial.published_lsn = 2;
        initial.txn = 1;
        let shared = Arc::new(Mutex::new(initial));
        let mut machine =
            Machine::new(shared.clone(), disk.clone(), "db".into(), opts.clone()).unwrap();
        let content = crate::turn::encode(
            "a",
            "pending",
            0,
            &NewTurn { role: Role::User, content: vec![1], meta: vec![] },
            None,
        )
        .unwrap();
        let mut batch = PreparedBatch::new(
            vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content }],
            Durability::Durable,
        )
        .unwrap();
        assert!(matches!(machine.prepare(&mut batch), Prepare::Write));
        let retry = Key(1, "a".into(), "retry".into());
        assert!(matches!(
            shared.lock().unwrap().index.reserve(&retry, 1).unwrap_err().kind,
            ErrorKind::ResourceExhausted("hot_pages")
        ));
        let worker =
            crate::checkpoint::Worker::start(disk, "db".into(), shared.clone(), &opts).unwrap();
        assert_eq!(worker.flush().unwrap(), 2);
        let mut state = shared.lock().unwrap();
        assert!(!state.index.sessions[&published].pages[0].dirty);
        assert_eq!(state.index.dirty_bytes, 128);
        assert_eq!(state.index.reserve(&retry, 1).unwrap(), 1);
    }

    #[test]
    fn pending_retry_shares_group_sync_and_poison_hides_dedup_reservations() {
        for fail in [false, true] {
            let disk = Arc::new(SimDisk::new(7));
            let opts = EngineOptions::default();
            let state = Arc::new(Mutex::new(State::new(&opts)));
            let machine =
                Machine::new(state.clone(), disk.clone(), "db".into(), opts.clone()).unwrap();
            let (mut core, _) = WriterCore::open_with(
                machine,
                disk.clone(),
                Arc::new(SimClock::default()),
                "db",
                opts.tape,
            )
            .unwrap();
            let payload = crate::turn::encode(
                "a",
                "s",
                0,
                &NewTurn { role: Role::User, content: b"hello".to_vec(), meta: vec![] },
                Some("key"),
            )
            .unwrap();
            let turn = RecordInput { kind: Kind::Turn, tenant_id: 1, content: payload };
            let mut replies = Vec::new();
            for records in [
                vec![
                    RecordInput {
                        kind: Kind::TenantCreate,
                        tenant_id: 0,
                        content: b"tenant".to_vec(),
                    },
                    turn.clone(),
                ],
                vec![turn.clone()],
            ] {
                let batch = PreparedBatch::new(records, Durability::Durable).unwrap();
                let (request, reply) = WriteRequest::new(batch);
                core.enqueue(request);
                replies.push(reply);
            }
            if fail {
                disk.fail_next_sync().unwrap();
            }
            core.step();
            let results: Vec<_> = replies.into_iter().map(|reply| reply.recv().unwrap()).collect();
            if fail {
                assert!(results
                    .iter()
                    .all(|r| r.as_ref().unwrap_err().outcome == Outcome::Unknown));
                assert!(core.is_poisoned());
                let batch = PreparedBatch::new(vec![turn], Durability::Durable).unwrap();
                let (request, reply) = WriteRequest::new(batch);
                core.enqueue(request);
                core.step();
                assert_eq!(reply.recv().unwrap().unwrap_err().outcome, Outcome::NotCommitted);
                let state = state.lock().unwrap();
                assert_eq!(state.published_lsn, 0);
                assert!(state.dedup.entries.is_empty());
                let entry = &state.index.sessions[&Key(1, "a".into(), "s".into())];
                assert_eq!(entry.reserved, 1);
                assert_eq!(entry.len, 0);
            } else {
                let Committed::Durable { txn, first_lsn, last_lsn, mut reply } =
                    results[0].clone().unwrap();
                assert_eq!(reply.len(), 8, "a fresh reply is the bare sequence");
                reply.push(REPLY_DEDUPLICATED);
                let cached = Committed::Durable { txn, first_lsn, last_lsn, reply };
                assert_eq!(results[1].as_ref().unwrap(), &cached);
                assert_eq!(core.durable_lsn(), 2);
                assert_eq!(state.lock().unwrap().dedup.entries.len(), 1);
            }
        }
    }
}
