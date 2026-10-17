//! Seeded, single-threaded crash workloads for the production writer state machine.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use zerodb_tape::dir::DataDir;
use zerodb_tape::frame::{self, Digest, Kind};
use zerodb_tape::io::{FileId, Io, LockGuard, SimClock};
use zerodb_tape::simdisk::SimDisk;
use zerodb_tape::writer::{PreparedBatch, WriteRequest, WriteResult, WriterCore};
use zerodb_tape::{Committed, Durability, Error, Options, Outcome, RecordInput, Result, Tape};

/// SplitMix64 keeps workload generation independent from disk crash randomness.
#[derive(Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        for chunk in bytes.chunks_mut(8) {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes()[..chunk.len()]);
        }
        bytes
    }
}

/// Captures one crash image at a chosen I/O boundary, without unwinding the writer.
///
/// Replies produced after that boundary are ignored by the oracle.
#[derive(Debug)]
struct CrashIo {
    disk: SimDisk,
    cut: AtomicUsize,
    fail_sync: AtomicUsize,
    image: Mutex<Option<SimDisk>>,
}

impl CrashIo {
    fn new(disk: SimDisk) -> Self {
        Self {
            disk,
            cut: AtomicUsize::new(0),
            fail_sync: AtomicUsize::new(0),
            image: Mutex::new(None),
        }
    }

    fn tick(counter: &AtomicUsize) -> bool {
        let n = counter.load(Ordering::SeqCst);
        n != 0 && counter.fetch_sub(1, Ordering::SeqCst) == 1
    }

    fn after<T>(&self, result: Result<T>) -> Result<T> {
        if Self::tick(&self.cut) {
            *self.image.lock().unwrap() = Some(self.disk.crash()?);
        }
        result
    }

    fn before_sync(&self) -> Result<()> {
        if Self::tick(&self.fail_sync) {
            self.disk.fail_next_sync()?;
        }
        Ok(())
    }

    fn cut_happened(&self) -> bool {
        self.image.lock().unwrap().is_some()
    }
}

impl Io for CrashIo {
    fn create(&self, path: &Path) -> Result<FileId> {
        self.after(self.disk.create(path))
    }
    fn open(&self, path: &Path) -> Result<FileId> {
        self.disk.open(path)
    }
    fn close(&self, file: FileId) -> Result<()> {
        self.disk.close(file)
    }
    fn write_at(&self, file: FileId, off: u64, bytes: &[u8]) -> Result<()> {
        self.after(self.disk.write_at(file, off, bytes))
    }
    fn read_at(&self, file: FileId, off: u64, bytes: &mut [u8]) -> Result<usize> {
        self.disk.read_at(file, off, bytes)
    }
    fn len(&self, file: FileId) -> Result<u64> {
        self.disk.len(file)
    }
    fn set_len(&self, file: FileId, len: u64) -> Result<()> {
        self.after(self.disk.set_len(file, len))
    }
    fn sync_data(&self, file: FileId) -> Result<()> {
        self.before_sync()?;
        self.after(self.disk.sync_data(file))
    }
    fn sync_dir(&self, path: &Path) -> Result<()> {
        self.before_sync()?;
        self.after(self.disk.sync_dir(path))
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.after(self.disk.rename(from, to))
    }
    fn remove(&self, path: &Path) -> Result<()> {
        self.after(self.disk.remove(path))
    }
    fn list(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.disk.list(path)
    }
    fn exists(&self, path: &Path) -> Result<bool> {
        self.disk.exists(path)
    }
    fn try_lock_exclusive(&self, path: &Path) -> Result<LockGuard> {
        self.disk.try_lock_exclusive(path)
    }
}

#[derive(Debug)]
struct Batch {
    records: Vec<RecordInput>,
    digests: Vec<Digest>,
}

#[derive(Debug, Default)]
struct Oracle {
    batches: Vec<Batch>,
    durable: BTreeMap<u64, Digest>,
}

impl Oracle {
    fn reply(&mut self, batch: Batch, reply: Option<WriteResult>) {
        match reply {
            Some(Ok(Committed::Durable { first_lsn, last_lsn, .. })) => {
                assert_eq!(last_lsn - first_lsn + 1, batch.records.len() as u64);
                for (i, digest) in batch.digests.iter().enumerate() {
                    assert!(self.durable.insert(first_lsn + i as u64, *digest).is_none());
                }
            }
            Some(Err(error)) if error.outcome == Outcome::NotCommitted => return,
            _ => {}
        }
        self.batches.push(batch);
    }

    fn verify(&mut self, tape: &Tape) {
        let candidates: BTreeMap<_, _> = self
            .batches
            .iter()
            .enumerate()
            .flat_map(|(b, batch)| {
                batch.digests.iter().enumerate().map(move |(r, digest)| (*digest, (b, r)))
            })
            .collect();
        let mut seen = vec![Vec::new(); self.batches.len()];
        let mut heads = BTreeMap::new();
        let mut observed = BTreeMap::new();
        let mut last_txn = 0;
        for lsn in 1..=tape.durable_lsn() {
            let record = tape.read(lsn).unwrap().unwrap();
            let h = &record.header;
            observed.insert(lsn, h.content_digest);
            assert_eq!(h.lsn, lsn, "LSNs must strictly increase");
            let &(b, r) = candidates.get(&h.content_digest).unwrap();
            let expected = &self.batches[b].records[r];
            assert_eq!(h.kind, expected.kind);
            assert_eq!(h.tenant_id, expected.tenant_id);
            assert_eq!(&record.payload[16..], expected.content);
            let salt: [u8; 16] = record.payload[..16].try_into().unwrap();
            assert_eq!(frame::content_digest(&salt, &expected.content), h.content_digest);
            if r == 0 {
                assert!(h.txn > last_txn, "transactions must strictly increase");
                last_txn = h.txn;
            } else {
                assert_eq!(seen[b].last(), Some(&(lsn - 1, h.txn, r - 1)));
                assert_eq!(h.txn, last_txn);
            }
            seen[b].push((lsn, h.txn, r));
            if h.kind != Kind::Cache {
                let prev = heads.entry(h.tenant_id).or_insert([0; 32]);
                assert_eq!(h.tenant_prev_chain, *prev);
                *prev = frame::chain_next(prev, h, &h.content_digest);
            } else {
                assert_eq!(h.tenant_prev_chain, [0; 32]);
            }
        }
        for (batch, seen) in self.batches.iter().zip(seen) {
            assert!(
                seen.is_empty() || seen.len() == batch.records.len(),
                "partial transaction became visible"
            );
        }
        for (lsn, digest) in &self.durable {
            assert_eq!(observed.get(lsn), Some(digest));
        }
        // Recovery syncs complete survivors. They must survive every later generation too.
        self.durable.extend(observed);
    }
}

fn enqueue(
    core: &mut WriterCore,
    records: Vec<RecordInput>,
    mode: Durability,
    rng: &mut Rng,
) -> (Batch, mpsc::Receiver<WriteResult>) {
    let mut digests = Vec::new();
    let mut index = 0;
    let prepared = PreparedBatch::with_salts(records.clone(), mode, || {
        let salt: [u8; 16] = rng.bytes(16).try_into().unwrap();
        digests.push(frame::content_digest(&salt, &records[index].content));
        index += 1;
        Ok(salt)
    })
    .unwrap();
    let (request, receiver) = WriteRequest::new(prepared);
    core.enqueue(request);
    (Batch { records, digests }, receiver)
}

fn probe(core: &mut WriterCore, oracle: &mut Oracle, rng: &mut Rng) {
    let records = vec![RecordInput { tenant_id: 1, kind: Kind::Turn, content: rng.bytes(32) }];
    let (batch, receiver) = enqueue(core, records, Durability::Durable, rng);
    let before = core.durable_lsn();
    core.step();
    let reply = receiver.recv().unwrap();
    assert!(matches!(reply, Ok(Committed::Durable { .. })), "{reply:?}");
    assert!(core.durable_lsn() > before, "reopened writer must accept writes");
    oracle.reply(batch, Some(reply));
}

fn workload(rng: &mut Rng, tenants: usize) -> (Vec<RecordInput>, Durability) {
    let mode = if rng.below(4) == 0 { Durability::Fast } else { Durability::Durable };
    let count = 1 + rng.below(64);
    let small = rng.below(2) == 0;
    let large = rng.below(80) == 0;
    let records = (0..count)
        .map(|i| {
            let len = if large && i == 0 {
                1024 * 1024 + rng.below(1024)
            } else if small {
                rng.below(257)
            } else {
                rng.below(8193)
            };
            let kind = if mode == Durability::Fast {
                Kind::Cache
            } else {
                [Kind::Turn, Kind::Fact, Kind::Cache, Kind::Redaction][rng.below(4)]
            };
            RecordInput { kind, tenant_id: 1 + rng.below(tenants) as u32, content: rng.bytes(len) }
        })
        .collect();
    (records, mode)
}

/// Runs 2 or 3 crash generations, retaining acknowledged and uncertain transactions in an oracle.
pub fn run_seed(seed: u64) {
    if seed % 97 == 1 {
        large_record(seed);
    }
    let mut rng = Rng::new(seed);
    let tenants = 1 + rng.below(8);
    let generations = 2 + rng.below(2);
    let opts = Options { segment_size: 64 * 1024, ..Options::default() };
    let clock = Arc::new(SimClock::default());
    let mut disk = SimDisk::new(seed);
    let mut oracle = Oracle::default();
    for generation in 0..generations {
        let io = Arc::new(CrashIo::new(disk));
        let (mut core, _) =
            WriterCore::open(io.clone(), clock.clone(), "db", opts.clone()).unwrap();
        probe(&mut core, &mut oracle, &mut rng);
        clock.advance(1_000_000_000);
        core.update_superblock().unwrap();
        let steps = 4 + rng.below(9);
        let cut_step = rng.below(steps);
        for step in 0..steps {
            let mut pending = Vec::new();
            for _ in 0..1 + rng.below(4) {
                let (records, mode) = workload(&mut rng, tenants);
                pending.push(enqueue(&mut core, records, mode, &mut rng));
            }
            if step == cut_step {
                io.cut.store(1 + rng.below(35), Ordering::SeqCst);
            }
            match rng.below(10) {
                0 => io.fail_sync.store(1 + rng.below(6), Ordering::SeqCst),
                1 => io.disk.enospc_after_bytes(rng.below(80 * 1024)).unwrap(),
                _ => {}
            }
            let skip_step = step == cut_step && rng.below(8) == 0;
            if !skip_step {
                core.step();
            }
            let cut = io.cut_happened() || skip_step;
            for (batch, receiver) in pending {
                let reply = if cut { None } else { Some(receiver.recv().unwrap()) };
                oracle.reply(batch, reply);
            }
            if cut {
                break;
            }
            clock.advance(rng.next_u64() % 1_500_000_000);
            let _ = core.update_superblock();
            if io.cut_happened() || core.is_poisoned() {
                break;
            }
        }
        disk = io.image.lock().unwrap().take().unwrap_or_else(|| io.disk.crash().unwrap());
        drop(core);
        if seed.is_multiple_of(11) && generation + 1 == generations {
            let dir = DataDir::open(Arc::new(disk.clone()), "db", None, opts.segment_size).unwrap();
            assert!(dir.superblock() > 0);
            let first = dir.segment_path(dir.segments()[0]);
            let file = disk.open(&first).unwrap();
            disk.flip_bit(file, rng.below(104) as u64).unwrap();
            disk.close(file).unwrap();
            drop(dir);
            assert!(matches!(
                Tape::open(Arc::new(disk), clock, "db", opts),
                Err(Error::CorruptionDetected { .. })
            ));
            return;
        }
        let (tape, report) =
            Tape::open(Arc::new(disk.clone()), clock.clone(), "db", opts.clone()).unwrap();
        oracle.verify(&tape);
        assert_eq!(report.records as u64, tape.durable_lsn());
        drop(tape);
    }
    let (mut core, _) =
        WriterCore::open(Arc::new(disk.clone()), clock.clone(), "db", opts.clone()).unwrap();
    probe(&mut core, &mut oracle, &mut rng);
    drop(core);
    let (tape, _) = Tape::open(Arc::new(disk), clock, "db", opts).unwrap();
    oracle.verify(&tape);
}

fn large_record(seed: u64) {
    // A 64 KiB segment rejects 1 MiB batches. Also exercise accepted large records separately.
    let mut rng = Rng::new(seed);
    let opts = Options { segment_size: 2 * 1024 * 1024, ..Options::default() };
    let disk = Arc::new(SimDisk::new(seed));
    let clock = Arc::new(SimClock::default());
    let (mut core, _) =
        WriterCore::open(disk.clone(), clock.clone(), "large", opts.clone()).unwrap();
    let mut oracle = Oracle::default();
    for _ in 0..2 {
        let records =
            vec![RecordInput { kind: Kind::Fact, tenant_id: 1, content: rng.bytes(1024 * 1024) }];
        let (batch, reply) = enqueue(&mut core, records, Durability::Durable, &mut rng);
        core.step();
        let reply = reply.recv().unwrap();
        assert!(matches!(reply, Ok(Committed::Durable { .. })));
        oracle.reply(batch, Some(reply));
    }
    drop(core);
    let disk = Arc::new(disk.crash().unwrap());
    let (tape, _) = Tape::open(disk.clone(), clock.clone(), "large", opts.clone()).unwrap();
    oracle.verify(&tape);
    drop(tape);
    let (mut core, _) =
        WriterCore::open(disk.clone(), clock.clone(), "large", opts.clone()).unwrap();
    probe(&mut core, &mut oracle, &mut rng);
    drop(core);
    let (tape, _) = Tape::open(Arc::new(disk.crash().unwrap()), clock, "large", opts).unwrap();
    oracle.verify(&tape);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_mutated_oracle(mutate: impl FnOnce(&mut Oracle)) {
        let disk = Arc::new(SimDisk::new(1));
        let clock = Arc::new(SimClock::default());
        let opts = Options { segment_size: 64 * 1024, ..Options::default() };
        let (mut core, _) =
            WriterCore::open(disk.clone(), clock.clone(), "db", opts.clone()).unwrap();
        let mut oracle = Oracle::default();
        probe(&mut core, &mut oracle, &mut Rng::new(1));
        mutate(&mut oracle);
        drop(core);
        let (tape, _) = Tape::open(disk, clock, "db", opts).unwrap();
        oracle.verify(&tape);
    }

    #[test]
    #[should_panic(expected = "partial transaction became visible")]
    fn oracle_rejects_partial_uncertain_transaction() {
        check_mutated_oracle(|oracle| {
            oracle.durable.clear();
            let batch = &mut oracle.batches[0];
            batch.records.push(batch.records[0].clone());
            batch.digests.push([0xff; 32]);
        });
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn oracle_rejects_changed_uncertain_content() {
        check_mutated_oracle(|oracle| {
            oracle.durable.clear();
            oracle.batches[0].records[0].content[0] ^= 1;
        });
    }
}
