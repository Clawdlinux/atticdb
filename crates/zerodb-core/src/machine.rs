use crate::checkpoint::{read_page, Command};
use crate::dedup::{digest, Dedup, DedupKey, Entry, Scope};
use crate::error::{lock, Result};
use crate::fact;
use crate::facts::{current, FactKey, Rev};
use crate::session::{address, Key};
use crate::state::State;
use crate::tenant::Registry;
use crate::turn::{decode_ref as decode, set_seq};
use crate::{EngineError, EngineOptions, ErrorKind};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use zerodb_tape::{
    Committed, CommittedRecord, Error, Io, Kind, Outcome, Prepare, PreparedBatch, StateMachine,
    WriteError,
};

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
        // A fact write is exactly one record. Turns and facts never share a batch.
        let (session, fact, dedup_key, digest) = if head.kind == Kind::Fact {
            let fact = fact::decode(&head.content)?;
            if body.len() != 1
                || fact.key.is_empty()
                || !fact.source_refs.is_empty()
                || (fact.tombstone && !(fact.content.is_empty() && fact.meta.is_empty()))
            {
                return Err(invalid());
            }
            let digest = batch.application_digest().unwrap_or_else(|| {
                fact.idem_key.as_ref().map_or([0; 32], |_| digest([head.content.as_slice()]))
            });
            let dedup_key =
                fact.idem_key.clone().map(|id| DedupKey(tenant, Scope::Fact(fact.key.clone()), id));
            (None, Some(fact), dedup_key, digest)
        } else {
            let first = decode(&head.content)?;
            let key = Key(tenant, first.agent_id.to_owned(), first.session_id.to_owned());
            let mut hash = (first.idem_key.is_some() && batch.application_digest().is_none())
                .then(blake3::Hasher::new);
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
                if let Some(hash) = &mut hash {
                    hash.update(&(record.content.len() as u32).to_le_bytes());
                    hash.update(&record.content);
                    #[cfg(test)]
                    {
                        self.prepare_hash_bytes += record.content.len();
                    }
                }
            }
            let digest = batch
                .application_digest()
                .or_else(|| hash.map(|h| *h.finalize().as_bytes()))
                .unwrap_or([0; 32]);
            let dedup_key = first
                .idem_key
                .map(|id| DedupKey(tenant, Scope::Session(key.2.clone()), id.to_owned()));
            (Some((key, body.len())), None, dedup_key, digest)
        };
        let mut state = lock(&self.state)?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if let Some(entry) =
            dedup_key.as_ref().and_then(|key| self.dedup.get(key).or_else(|| state.dedup.get(key)))
        {
            let Committed::Durable { txn, first_lsn, last_lsn, mut reply } =
                entry.resolve(digest)?;
            reply.push(REPLY_DEDUPLICATED);
            return Ok(Prepare::Reply(Committed::Durable { txn, first_lsn, last_lsn, reply }));
        }
        if dedup_key.is_some() && !self.dedup.can_stage(tenant) {
            return Err(EngineError::rejected(ErrorKind::ResourceExhausted("prepared_dedup")));
        }
        let fact = fact.map(|fact| ((tenant, fact.key), fact.tombstone));
        if let Some((key, _)) = &fact {
            let newest =
                self.facts.get(key).or_else(|| state.facts.map.get(key).and_then(|r| r.last()));
            if batch.application_arg().is_some_and(|lsn| lsn != current(newest)) {
                return Err(EngineError::rejected(ErrorKind::Conflict));
            }
            if newest.is_none() && state.facts.full(&self.facts) {
                return Err(EngineError::rejected(ErrorKind::ResourceExhausted("facts")));
            }
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
        if let Some(key) = dedup_key {
            self.dedup.insert(Entry {
                key,
                digest,
                committed: Committed::Durable { txn, first_lsn, last_lsn, reply },
            });
        }
        if let Some((key, deleted)) = fact {
            self.facts.insert(key, Rev { lsn: last_lsn, deleted });
        }
        Ok(Prepare::Write)
    }

    fn install(
        &mut self,
        state: &mut State,
        record: &CommittedRecord,
        floor: u64,
        replay: bool,
    ) -> Result<()> {
        let corrupt = || EngineError::rejected(ErrorKind::CorruptIndex);
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
                    let mut normalized = record.payload.to_vec();
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
                let fact = fact::decode(&record.payload).map_err(|_| corrupt())?;
                let key = (record.tenant_id, fact.key);
                state.facts.install(
                    &key,
                    Rev { lsn: record.lsn, deleted: fact.tombstone },
                    floor,
                )?;
                if self.facts.get(&key).is_some_and(|rev| rev.lsn == record.lsn) {
                    self.facts.remove(&key);
                }
                if let Some(id) = fact.idem_key.filter(|_| replay) {
                    let key = DedupKey(record.tenant_id, Scope::Fact(key.1), id);
                    self.rebuild(state, record, key, &record.payload, Vec::new());
                }
            }
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
        state.bytes = state.bytes.saturating_add(
            record.payload.len() as u64 + 40 + if state.txn != record.txn { 72 } else { 0 },
        );
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
        state.dedup.insert(Entry {
            key,
            digest: *self.hash.finalize().as_bytes(),
            committed: Committed::Durable {
                txn: record.txn,
                first_lsn: self.first_lsn,
                last_lsn: record.lsn,
                reply,
            },
        });
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
    fn replay(&mut self, record: &CommittedRecord) -> zerodb_tape::Result<()> {
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
) -> zerodb_tape::Result<()> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("zerodb-checkpoint-recovery".into())
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
    use zerodb_tape::sim::{SimClock, WriteRequest, WriterCore};
    use zerodb_tape::{Durability, Kind, RecordInput, SimDisk};

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
        for (case, records) in [
            ("forget", vec![(Kind::Forget, &turn)]),
            ("fact then turn", vec![(Kind::Fact, &fact), (Kind::Turn, &turn)]),
            ("turn then fact", vec![(Kind::Turn, &turn), (Kind::Fact, &fact)]),
            ("one key twice", vec![(Kind::Fact, &fact), (Kind::Fact, &fact)]),
        ] {
            let opts = EngineOptions::default();
            let mut initial = State::new(&opts);
            initial.tenants.insert("t", 1, 1, 10_000).unwrap();
            let mut machine = Machine::new(
                Arc::new(Mutex::new(initial)),
                Arc::new(SimDisk::new(0)),
                "db".into(),
                opts,
            )
            .unwrap();
            let records = records
                .into_iter()
                .map(|(kind, content)| RecordInput { kind, tenant_id: 1, content: content.clone() })
                .collect();
            let mut batch = PreparedBatch::new(records, Durability::Durable).unwrap();
            let Prepare::Reject(error) = machine.prepare(&mut batch) else {
                panic!("{case}: expected rejection");
            };
            assert!(matches!(EngineError::write(error).kind, ErrorKind::InvalidInput), "{case}");
            assert_eq!(machine.lsn, 0, "{case}");
            assert!(machine.facts.is_empty() && machine.dedup.entries.is_empty(), "{case}");
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
            initial.dedup.insert(Entry {
                key: DedupKey(1, Scope::Session(age.to_string()), "old".into()),
                digest: [0; 32],
                committed: Committed::Durable {
                    txn: 1,
                    first_lsn: age,
                    last_lsn: age,
                    reply: 1u64.to_le_bytes().to_vec(),
                },
            });
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
