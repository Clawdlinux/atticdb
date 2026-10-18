use crate::checkpoint::{read_page, Command};
use crate::dedup::{Dedup, DedupKey, Entry};
use crate::error::{lock, Result};
use crate::session::{address, Key};
use crate::state::State;
use crate::tenant::Registry;
use crate::turn::{decode, set_seq};
use crate::{EngineError, EngineOptions, ErrorKind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use zerodb_tape::{
    Committed, CommittedRecord, Error, Io, Outcome, Prepare, PreparedBatch, StateMachine,
    WriteError,
};

pub(crate) struct Machine {
    state: Arc<Mutex<State>>,
    io: Arc<dyn Io>,
    dir: PathBuf,
    opts: EngineOptions,
    tenants: Registry,
    dedup: Dedup,
    lsn: u64,
    txn: u64,
    digest_txn: u64,
    first_lsn: u64,
    hash: blake3::Hasher,
}
impl Machine {
    pub(crate) fn new(
        state: Arc<Mutex<State>>,
        io: Arc<dyn Io>,
        dir: PathBuf,
        opts: EngineOptions,
    ) -> Result<Self> {
        let s = lock(&state)?;
        let machine = Self {
            state: state.clone(),
            io,
            dir,
            opts,
            tenants: s.tenants.clone(),
            dedup: s.dedup.clone(),
            lsn: s.published_lsn,
            txn: s.txn,
            digest_txn: 0,
            first_lsn: 0,
            hash: blake3::Hasher::new(),
        };
        Ok(machine)
    }
    fn prepare_inner(&mut self, batch: &mut PreparedBatch) -> Result<Prepare> {
        let records: Vec<_> = batch.records().collect();
        let create = records.first().filter(|r| r.kind == zerodb_tape::Kind::TenantCreate);
        let turns = &records[usize::from(create.is_some())..];
        let first = decode(
            &turns.first().ok_or_else(|| EngineError::rejected(ErrorKind::InvalidInput))?.content,
        )?;
        let tenant = if let Some(record) = create {
            let name = std::str::from_utf8(&record.content)
                .map_err(|_| EngineError::rejected(ErrorKind::InvalidInput))?;
            if name.len() > 255 || self.tenants.get(name).is_some() {
                return Err(EngineError::rejected(ErrorKind::InvalidInput));
            }
            if self.tenants.entries.len() >= self.opts.max_tenants.min(10_000) {
                return Err(EngineError::rejected(ErrorKind::ResourceExhausted("tenants")));
            }
            self.tenants.entries.len() as u32 + 1
        } else {
            turns[0].tenant_id
        };
        if tenant == 0 || (create.is_none() && tenant as usize > self.tenants.entries.len()) {
            return Err(EngineError::rejected(ErrorKind::InvalidInput));
        }
        let key = Key(tenant, first.agent_id, first.session_id.clone());
        let mut hash = blake3::Hasher::new();
        for record in turns {
            let decoded = decode(&record.content)?;
            if record.kind != zerodb_tape::Kind::Turn
                || decoded.seq != 0
                || decoded.agent_id != key.1
                || decoded.session_id != key.2
                || decoded.idem_key != first.idem_key
                || (create.is_none() && record.tenant_id != tenant)
            {
                return Err(EngineError::rejected(ErrorKind::InvalidInput));
            }
            hash.update(&(record.content.len() as u32).to_le_bytes());
            hash.update(&record.content);
        }
        let digest = *hash.finalize().as_bytes();
        let dedup_key = first.idem_key.map(|id| DedupKey(tenant, first.session_id, id));
        if let Some(entry) = dedup_key.as_ref().and_then(|key| self.dedup.get(key)) {
            return if entry.digest == digest {
                Ok(Prepare::Reply(entry.committed.clone()))
            } else {
                Err(EngineError::rejected(ErrorKind::IdempotencyConflict))
            };
        }
        let first_lsn = self
            .lsn
            .checked_add(1)
            .ok_or_else(|| EngineError::rejected(ErrorKind::ResourceExhausted("lsn")))?;
        let last_lsn = self
            .lsn
            .checked_add(records.len() as u64)
            .ok_or_else(|| EngineError::rejected(ErrorKind::ResourceExhausted("lsn")))?;
        let txn = self
            .txn
            .checked_add(1)
            .ok_or_else(|| EngineError::rejected(ErrorKind::ResourceExhausted("txn")))?;
        let mut state = lock(&self.state)?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        let seq = state.index.reserve(&key, turns.len())?;
        if let Some(record) = create {
            let name = std::str::from_utf8(&record.content)
                .map_err(|_| EngineError::rejected(ErrorKind::InvalidInput))?;
            self.tenants.insert(name, tenant, first_lsn, self.opts.max_tenants)?;
        }
        let offset = usize::from(create.is_some());
        for (index, record) in batch.records_mut().enumerate() {
            record.tenant_id = tenant;
            if index >= offset {
                set_seq(&mut record.content, seq + (index - offset) as u64)?;
            }
        }
        self.lsn = last_lsn;
        self.txn = txn;
        if let Some(key) = dedup_key {
            self.dedup.insert(Entry {
                key,
                digest,
                committed: Committed::Durable { txn, first_lsn, last_lsn },
            });
        }
        Ok(Prepare::Write)
    }

    fn install(&mut self, state: &mut State, record: &CommittedRecord, replay: bool) -> Result<()> {
        if self.digest_txn != record.txn {
            self.digest_txn = record.txn;
            self.first_lsn = record.lsn;
            self.hash.reset();
        }
        match record.kind {
            zerodb_tape::Kind::TenantCreate => {
                let name = std::str::from_utf8(&record.payload)
                    .map_err(|_| EngineError::rejected(ErrorKind::CorruptIndex))?;
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
            zerodb_tape::Kind::Turn => {
                let turn = decode(&record.payload)?;
                let key = Key(record.tenant_id, turn.agent_id, turn.session_id.clone());
                if record.tenant_id == 0 || record.tenant_id as usize > state.tenants.entries.len()
                {
                    return Err(EngineError::rejected(ErrorKind::CorruptIndex));
                }
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
                if let Some(id) = turn.idem_key {
                    let mut normalized = record.payload.to_vec();
                    set_seq(&mut normalized, 0)?;
                    self.hash.update(&(normalized.len() as u32).to_le_bytes());
                    self.hash.update(&normalized);
                    let entry = Entry {
                        key: DedupKey(record.tenant_id, turn.session_id, id),
                        digest: *self.hash.finalize().as_bytes(),
                        committed: Committed::Durable {
                            txn: record.txn,
                            first_lsn: self.first_lsn,
                            last_lsn: record.lsn,
                        },
                    };
                    state.dedup.insert(entry.clone());
                    if replay {
                        self.dedup.insert(entry);
                    }
                }
            }
            _ => return Err(EngineError::rejected(ErrorKind::CorruptIndex)),
        }
        state.bytes = state.bytes.saturating_add(
            record.payload.len() as u64 + 120 + if state.txn != record.txn { 148 } else { 0 },
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
}
fn tape_error(error: EngineError) -> Error {
    match error.kind {
        ErrorKind::IdempotencyConflict => Error::Rejected("idempotency"),
        ErrorKind::ResourceExhausted(resource) => Error::Rejected(resource),
        ErrorKind::Storage(error) => error,
        _ => Error::InvalidFrame(error.to_string()),
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
        for record in records {
            if let Err(mut error) = self.install(&mut state, record, false) {
                error.outcome = Outcome::Committed;
                state.failure = Some(error);
                break;
            }
        }
        if state.bytes.saturating_sub(state.checkpoint_bytes) >= self.opts.checkpoint_bytes {
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
                std::thread::scope(|scope| {
                    std::thread::Builder::new()
                        .name("zerodb-checkpoint-recovery".into())
                        .spawn_scoped(scope, || {
                            crate::checkpoint::save(self.io.as_ref(), &self.dir, &shared)
                        })
                        .map_err(Error::from)?
                        .join()
                        .map_err(|_| Error::Poisoned)?
                        .map_err(tape_error)
                })?;
            }
            let mut state = lock(&shared).map_err(tape_error)?;
            self.install(&mut state, record, true).map_err(tape_error)
        })();
        if result.is_err() {
            if let Ok(mut state) = self.state.lock() {
                state.replay_failed = true;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{NewTurn, Role, Trust};
    use zerodb_tape::sim::{SimClock, WriteRequest, WriterCore};
    use zerodb_tape::{Durability, Kind, RecordInput, SimDisk};

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
                &NewTurn {
                    role: Role::User,
                    content: b"hello".to_vec(),
                    meta: vec![],
                    trust: Trust::Untrusted,
                },
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
                let batch = PreparedBatch::with_salts(records, Durability::Durable, || Ok([3; 16]))
                    .unwrap();
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
                let batch =
                    PreparedBatch::with_salts(vec![turn], Durability::Durable, || Ok([3; 16]))
                        .unwrap();
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
                assert_eq!(results[0].as_ref().unwrap(), results[1].as_ref().unwrap());
                assert_eq!(core.durable_lsn(), 2);
                assert_eq!(state.lock().unwrap().dedup.entries.len(), 1);
            }
        }
    }
}
