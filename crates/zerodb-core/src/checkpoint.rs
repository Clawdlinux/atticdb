use crate::dedup::{DedupKey, Entry};
use crate::error::{lock, Result};
use crate::session::{Backing, Key};
use crate::state::State;
use crate::turn::{bytes, Cursor};
use crate::{EngineError, EngineOptions, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use zerodb_tape::{Committed, FileId, Io};

fn corrupt() -> EngineError {
    EngineError::rejected(ErrorKind::CorruptIndex)
}

struct File<'a>(&'a dyn Io, FileId);
impl Drop for File<'_> {
    fn drop(&mut self) {
        let _ = self.0.close(self.1);
    }
}

fn crc(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}
fn exact(io: &dyn Io, id: FileId, mut off: u64, mut out: &mut [u8]) -> Result<()> {
    while !out.is_empty() {
        let n = io.read_at(id, off, out)?;
        if n == 0 {
            return Err(corrupt());
        }
        off += n as u64;
        out = &mut out[n..];
    }
    Ok(())
}
fn write(io: &dyn Io, id: FileId, off: &mut u64, payload: &[u8]) -> Result<(u64, u32)> {
    let mut record = vec![0; 4];
    record.extend_from_slice(&((payload.len() + 8) as u32).to_le_bytes());
    record.extend_from_slice(payload);
    let sum = crc(&record[4..]);
    record[..4].copy_from_slice(&sum.to_le_bytes());
    let location = (*off, record.len() as u32);
    io.write_at(id, *off, &record)?;
    *off += record.len() as u64;
    Ok(location)
}
fn read(io: &dyn Io, id: FileId, off: u64) -> Result<(Vec<u8>, u32)> {
    let mut header = [0; 8];
    exact(io, id, off, &mut header)?;
    let len = u32::from_le_bytes(header[4..].try_into().map_err(|_| corrupt())?);
    if !(9..=131072).contains(&len) {
        return Err(corrupt());
    }
    let mut data = vec![0; len as usize];
    data[..8].copy_from_slice(&header);
    exact(io, id, off + 8, &mut data[8..])?;
    if crc(&data[4..]) != u32::from_le_bytes(header[..4].try_into().map_err(|_| corrupt())?) {
        return Err(corrupt());
    }
    Ok((data[8..].to_vec(), len))
}
fn put_key(out: &mut Vec<u8>, key: &Key) {
    out.extend_from_slice(&key.0.to_le_bytes());
    bytes(out, key.1.as_bytes());
    bytes(out, key.2.as_bytes());
}
fn get_key(cur: &mut Cursor<'_>) -> Result<Key> {
    Ok(Key(cur.u32()?, cur.string(255)?, cur.string(255)?))
}
fn page(data: &[u8]) -> Result<(Key, usize, Vec<u64>)> {
    let mut cur = Cursor(data);
    if cur.byte()? != 2 {
        return Err(corrupt());
    }
    let key = get_key(&mut cur)?;
    let page = cur.u32()? as usize;
    let count = cur.u32()? as usize;
    if count == 0 || count > crate::session::capacity(page) {
        return Err(corrupt());
    }
    let slots = (0..count).map(|_| cur.u64()).collect::<std::result::Result<Vec<_>, _>>()?;
    cur.end()?;
    Ok((key, page, slots))
}
pub(crate) fn read_page(io: &dyn Io, dir: &Path, backing: &Backing) -> Result<Vec<u64>> {
    let file = File(io, io.open(&dir.join(&backing.file))?);
    let (data, len) = read(io, file.1, backing.offset)?;
    if len != backing.len {
        return Err(corrupt());
    }
    Ok(page(&data)?.2)
}

fn load_inner(io: &dyn Io, dir: &Path, opts: &EngineOptions) -> Result<State> {
    let mut state = State::new(opts);
    if !io.exists(&dir.join("CHECKPOINT"))? {
        return Ok(state);
    }
    let file = File(io, io.open(&dir.join("CHECKPOINT"))?);
    let (data, len) = read(io, file.1, 0)?;
    if io.len(file.1)? != u64::from(len) {
        return Err(corrupt());
    }
    let mut cur = Cursor(&data);
    if cur.take(4)? != b"ZCM1" {
        return Err(corrupt());
    }
    let watermark = cur.u64()?;
    let txn = cur.u64()?;
    let count = cur.byte()?;
    if count == 0 || count > 8 {
        return Err(corrupt());
    }
    for _ in 0..count {
        let name = cur.string(128)?;
        if !name.starts_with("ckpt-")
            || !name.ends_with(".zdb")
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        {
            return Err(corrupt());
        }
        state.chain.push(name);
    }
    cur.end()?;
    let mut previous = 0;
    for name in state.chain.clone() {
        let file = File(io, io.open(&dir.join(&name))?);
        let (header, header_len) = read(io, file.1, 0)?;
        let mut off = u64::from(header_len);
        let mut cur = Cursor(&header);
        if cur.take(4)? != b"ZCK1" {
            return Err(corrupt());
        }
        let boundary = cur.u64()?;
        let boundary_txn = cur.u64()?;
        cur.end()?;
        if boundary <= previous || boundary > watermark || boundary_txn > txn {
            return Err(corrupt());
        }
        previous = boundary;
        let file_len = io.len(file.1)?;
        while off < file_len {
            let (data, len) = read(io, file.1, off)?;
            let mut cur = Cursor(&data);
            match cur.byte()? {
                1 => {
                    let id = cur.u32()?;
                    let lsn = cur.u64()?;
                    let name = cur.string(255)?;
                    cur.end()?;
                    if lsn == 0 || lsn > boundary {
                        return Err(corrupt());
                    }
                    state.tenants.insert(&name, id, lsn, opts.max_tenants)?;
                }
                2 => {
                    let (key, index, slots) = page(&data)?;
                    if key.0 == 0
                        || key.0 as usize > state.tenants.entries.len()
                        || slots.iter().any(|&lsn| lsn == 0 || lsn > boundary)
                        || slots.windows(2).any(|w| w[0] >= w[1])
                    {
                        return Err(corrupt());
                    }
                    let revision = *slots.last().ok_or_else(corrupt)?;
                    state.index.restore(
                        key,
                        index,
                        slots.len(),
                        revision,
                        Backing { file: name.clone(), offset: off, len },
                    )?;
                }
                3 => {
                    let tenant = cur.u32()?;
                    let session = cur.string(255)?;
                    let key = cur.string(128)?;
                    let digest = cur.take(32)?.try_into().map_err(|_| corrupt())?;
                    let txn = cur.u64()?;
                    let first_lsn = cur.u64()?;
                    let last_lsn = cur.u64()?;
                    cur.end()?;
                    if first_lsn == 0
                        || first_lsn > last_lsn
                        || last_lsn > boundary
                        || tenant == 0
                        || tenant as usize > state.tenants.entries.len()
                    {
                        return Err(corrupt());
                    }
                    state.dedup.insert(Entry {
                        key: DedupKey(tenant, session, key),
                        digest,
                        committed: Committed::Durable { txn, first_lsn, last_lsn },
                    });
                }
                _ => return Err(corrupt()),
            }
            off = off.checked_add(u64::from(len)).ok_or_else(corrupt)?;
        }
        if off != file_len {
            return Err(corrupt());
        }
    }
    if previous != watermark {
        return Err(corrupt());
    }
    state.watermark = watermark;
    state.published_lsn = watermark;
    state.txn = txn;
    Ok(state)
}
pub(crate) fn load(io: &dyn Io, dir: &Path, opts: &EngineOptions) -> State {
    match load_inner(io, dir, opts) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("zerodb: checkpoint ignored, replaying tape: {error}");
            let mut state = State::new(opts);
            state.warnings = 1;
            state
        }
    }
}

type PageSnapshot = (Key, usize, u64, Option<Vec<u64>>, Option<Backing>);
struct Snapshot {
    lsn: u64,
    txn: u64,
    bytes: u64,
    full: bool,
    chain: Vec<String>,
    tenants: Vec<(u32, String, u64)>,
    pages: Vec<PageSnapshot>,
    dedup: Vec<Entry>,
}
fn snapshot(state: &State) -> Snapshot {
    let full = state.chain.len() >= 8;
    let mut pages = Vec::new();
    if full {
        for (key, entry) in &state.index.sessions {
            for (index, page) in entry.pages.iter().enumerate() {
                if page.revision > 0 {
                    pages.push((
                        key.clone(),
                        index,
                        page.revision,
                        page.slots.clone(),
                        page.backing.clone(),
                    ));
                }
            }
        }
        pages.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    } else {
        for (key, index) in &state.dirty {
            let page = &state.index.sessions[key].pages[*index];
            if page.revision > 0 {
                pages.push((
                    key.clone(),
                    *index,
                    page.revision,
                    page.slots.clone(),
                    page.backing.clone(),
                ));
            }
        }
    }
    Snapshot {
        lsn: state.published_lsn,
        txn: state.txn,
        bytes: state.bytes,
        full,
        chain: state.chain.clone(),
        tenants: state
            .tenants
            .entries
            .iter()
            .enumerate()
            .filter(|(_, (_, lsn))| full || *lsn > state.watermark)
            .map(|(i, (name, lsn))| (i as u32 + 1, name.clone(), *lsn))
            .collect(),
        pages,
        dedup: state
            .dedup
            .entries
            .values()
            .filter(|e| full || e.age() > state.watermark)
            .cloned()
            .collect(),
    }
}
pub(crate) fn save(io: &dyn Io, dir: &Path, shared: &Mutex<State>) -> Result<u64> {
    let snap = {
        let state = lock(shared)?;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.published_lsn == state.watermark {
            return Ok(state.watermark);
        }
        snapshot(&state)
    };
    let mut name = format!("ckpt-{}.zdb", snap.lsn);
    let mut attempt = 0;
    while io.exists(&dir.join(&name))? {
        attempt += 1;
        name = format!("ckpt-{}-{attempt}.zdb", snap.lsn);
    }
    let file = File(io, io.create(&dir.join(&name))?);
    let mut off = 0;
    let mut header = b"ZCK1".to_vec();
    header.extend_from_slice(&snap.lsn.to_le_bytes());
    header.extend_from_slice(&snap.txn.to_le_bytes());
    write(io, file.1, &mut off, &header)?;
    for (id, name, lsn) in &snap.tenants {
        let mut data = vec![1];
        data.extend_from_slice(&id.to_le_bytes());
        data.extend_from_slice(&lsn.to_le_bytes());
        bytes(&mut data, name.as_bytes());
        write(io, file.1, &mut off, &data)?;
    }
    let mut locations = Vec::new();
    for (key, index, revision, slots, backing) in snap.pages {
        let slots = match slots {
            Some(slots) => slots,
            None => read_page(io, dir, &backing.ok_or_else(corrupt)?)?,
        };
        let mut data = vec![2];
        put_key(&mut data, &key);
        data.extend_from_slice(&(index as u32).to_le_bytes());
        data.extend_from_slice(&(slots.len() as u32).to_le_bytes());
        for slot in slots {
            data.extend_from_slice(&slot.to_le_bytes());
        }
        let (offset, len) = write(io, file.1, &mut off, &data)?;
        locations.push((key, index, revision, Backing { file: name.clone(), offset, len }));
    }
    for entry in snap.dedup {
        let mut data = vec![3];
        data.extend_from_slice(&entry.key.0.to_le_bytes());
        bytes(&mut data, entry.key.1.as_bytes());
        bytes(&mut data, entry.key.2.as_bytes());
        data.extend_from_slice(&entry.digest);
        if let Committed::Durable { txn, first_lsn, last_lsn } = entry.committed {
            for value in [txn, first_lsn, last_lsn] {
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
        write(io, file.1, &mut off, &data)?;
    }
    io.sync_data(file.1)?;
    io.sync_dir(dir)?;
    let mut chain = if snap.full { Vec::new() } else { snap.chain.clone() };
    chain.push(name);
    let mut manifest = b"ZCM1".to_vec();
    manifest.extend_from_slice(&snap.lsn.to_le_bytes());
    manifest.extend_from_slice(&snap.txn.to_le_bytes());
    manifest.push(chain.len() as u8);
    for name in &chain {
        bytes(&mut manifest, name.as_bytes());
    }
    let next = dir.join("CHECKPOINT.next");
    if io.exists(&next)? {
        io.remove(&next)?;
    }
    let meta = File(io, io.create(&next)?);
    write(io, meta.1, &mut 0, &manifest)?;
    io.sync_data(meta.1)?;
    io.rename(&next, &dir.join("CHECKPOINT"))?;
    io.sync_dir(dir)?;
    let mut state = lock(shared)?;
    for (key, page, revision, backing) in locations {
        state.index.clean(&key, page, revision, backing);
        if state.index.sessions[&key].pages[page].revision == revision {
            state.dirty.remove(&(key, page));
        }
    }
    state.watermark = snap.lsn;
    state.checkpoint_bytes = snap.bytes;
    state.chain = chain;
    if snap.full {
        for name in snap.chain {
            let _ = io.remove(&dir.join(name));
        }
        let _ = io.sync_dir(dir);
    }
    Ok(snap.lsn)
}

#[derive(Debug)]
pub(crate) enum Command {
    Wake,
    Flush(mpsc::SyncSender<Result<u64>>),
    Stop,
}
pub(crate) struct Worker {
    pub(crate) sender: mpsc::SyncSender<Command>,
    thread: Option<JoinHandle<()>>,
}
impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointWorker").finish_non_exhaustive()
    }
}
impl Worker {
    pub(crate) fn start(
        io: Arc<dyn Io>,
        dir: PathBuf,
        state: Arc<Mutex<State>>,
        opts: &EngineOptions,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(16);
        let interval = opts.checkpoint_interval;
        let thread = std::thread::Builder::new()
            .name("zerodb-checkpoint".into())
            .spawn(move || loop {
                let command = receiver.recv_timeout(interval);
                if matches!(command, Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected))
                {
                    break;
                }
                let result = save(io.as_ref(), &dir, &state);
                match command {
                    Ok(Command::Flush(reply)) => {
                        let _ = reply.send(result);
                    }
                    _ => {
                        if let Err(error) = result {
                            eprintln!("zerodb: checkpoint failed: {error}");
                        }
                    }
                }
            })
            .map_err(zerodb_tape::Error::from)?;
        Ok(Self { sender, thread: Some(thread) })
    }
    pub(crate) fn flush(&self) -> Result<u64> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Flush(sender))
            .map_err(|_| EngineError::from(zerodb_tape::Error::Poisoned))?;
        receiver.recv().map_err(|_| EngineError::from(zerodb_tape::Error::Poisoned))?
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zerodb_tape::{LockGuard, SimDisk};

    fn fixture(disk: &SimDisk, shapes: &[(usize, usize)], boundary: u64) {
        let path = Path::new("db/ckpt-3.zdb");
        let id = if disk.exists(path).unwrap() {
            disk.open(path).unwrap()
        } else {
            disk.create(path).unwrap()
        };
        disk.set_len(id, 0).unwrap();
        let mut off = 0;
        let mut header = b"ZCK1".to_vec();
        header.extend_from_slice(&boundary.to_le_bytes());
        header.extend_from_slice(&1u64.to_le_bytes());
        write(disk, id, &mut off, &header).unwrap();
        let mut tenant = vec![1];
        tenant.extend_from_slice(&1u32.to_le_bytes());
        tenant.extend_from_slice(&1u64.to_le_bytes());
        bytes(&mut tenant, b"t");
        write(disk, id, &mut off, &tenant).unwrap();
        for &(page, count) in shapes {
            let mut data = vec![2];
            put_key(&mut data, &Key(1, "a".into(), "s".into()));
            data.extend_from_slice(&(page as u32).to_le_bytes());
            data.extend_from_slice(&(count as u32).to_le_bytes());
            for lsn in 2..count as u64 + 2 {
                data.extend_from_slice(&lsn.to_le_bytes());
            }
            write(disk, id, &mut off, &data).unwrap();
        }
        disk.sync_data(id).unwrap();
        disk.close(id).unwrap();
        let path = Path::new("db/CHECKPOINT");
        let id = if disk.exists(path).unwrap() {
            disk.open(path).unwrap()
        } else {
            disk.create(path).unwrap()
        };
        disk.set_len(id, 0).unwrap();
        let mut data = b"ZCM1".to_vec();
        data.extend_from_slice(&boundary.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes());
        data.push(1);
        bytes(&mut data, b"ckpt-3.zdb");
        write(disk, id, &mut 0, &data).unwrap();
        disk.sync_data(id).unwrap();
        disk.close(id).unwrap();
        disk.sync_dir(Path::new("db")).unwrap();
    }

    #[test]
    fn crc_valid_malformed_page_geometry_is_rejected() {
        for shape in [
            vec![(1, 1)],
            vec![(0, 1), (1, 1)],
            vec![(0, 16), (0, 1)],
            vec![(0, 16), (1, 1), (0, 1)],
        ] {
            let disk = SimDisk::new(4);
            fixture(&disk, &shape, 100);
            assert!(
                load_inner(&disk, Path::new("db"), &EngineOptions::default()).is_err(),
                "{shape:?}"
            );
        }
    }

    #[test]
    fn replay_failure_after_a_crc_valid_checkpoint_retries_from_zero() {
        use crate::{Engine, NewTurn, Role, Scope, Trust, WindowQuery};
        let disk = Arc::new(SimDisk::new(5));
        let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
        let turn =
            NewTurn { role: Role::User, content: vec![1], meta: vec![], trust: Trust::Untrusted };
        let engine = Engine::open_with(
            disk.clone(),
            Arc::new(zerodb_tape::sim::SimClock::default()),
            "db",
            EngineOptions::default(),
        )
        .unwrap();
        engine.append(&scope, vec![turn.clone(); 2], None).unwrap();
        engine.checkpoint().unwrap();
        engine.append(&scope, vec![turn], None).unwrap();
        drop(engine);
        fixture(&disk, &[(0, 1)], 3);
        let engine = Engine::open_with(
            disk,
            Arc::new(zerodb_tape::sim::SimClock::default()),
            "db",
            EngineOptions::default(),
        )
        .unwrap();
        assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 1);
        assert_eq!(engine.metrics().unwrap().replayed_records, 4);
        assert_eq!(engine.window(&scope, WindowQuery::Last(10)).unwrap().len(), 3);
    }

    fn append(state: &Mutex<State>, seq: u64) {
        let mut state = state.lock().unwrap();
        let key = Key(1, "a".into(), "s".into());
        state.index.reserve(&key, 1).unwrap();
        state.index.publish(&key, seq, seq + 1).unwrap();
        state.dirty.insert((key, crate::session::address(seq).0));
        state.published_lsn = seq + 1;
        state.txn = seq;
    }
    fn state() -> Mutex<State> {
        let mut state = State::new(&EngineOptions::default());
        state.tenants.insert("t", 1, 1, 10_000).unwrap();
        Mutex::new(state)
    }

    #[test]
    fn deltas_compaction_cold_reload_and_corruption_fallback() {
        let disk = SimDisk::new(1);
        let dir = Path::new("db");
        let shared = state();
        for seq in 1..=10 {
            append(&shared, seq);
            save(&disk, dir, &shared).unwrap();
            let loaded = load(&disk, dir, &EngineOptions::default());
            assert_eq!(loaded.watermark, seq + 1);
            assert_eq!(loaded.index.sessions[&Key(1, "a".into(), "s".into())].len, seq);
        }
        let state = shared.lock().unwrap();
        assert_eq!(state.chain.len(), 2);
        let backing = state.index.sessions[&Key(1, "a".into(), "s".into())].pages[0]
            .backing
            .as_ref()
            .unwrap();
        assert_eq!(read_page(&disk, dir, backing).unwrap(), (2..=11).collect::<Vec<_>>());
        let id = disk.open(&dir.join(&state.chain[0])).unwrap();
        disk.flip_bit(id, 12).unwrap();
        disk.close(id).unwrap();
        let loaded = load(&disk, dir, &EngineOptions::default());
        assert_eq!(loaded.watermark, 0);
        assert_eq!(loaded.warnings, 1);
    }

    struct FaultIo {
        disk: SimDisk,
        at: usize,
        count: AtomicUsize,
        crashed: Mutex<Option<SimDisk>>,
    }
    impl FaultIo {
        fn hit<T>(&self, result: zerodb_tape::Result<T>) -> zerodb_tape::Result<T> {
            let value = result?;
            if self.count.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
                *self.crashed.lock().unwrap() = Some(self.disk.crash()?);
                return Err(zerodb_tape::Error::Io(
                    std::io::ErrorKind::Interrupted,
                    "crash".into(),
                ));
            }
            Ok(value)
        }
    }
    impl Io for FaultIo {
        fn create(&self, p: &Path) -> zerodb_tape::Result<FileId> {
            self.hit(self.disk.create(p))
        }
        fn open(&self, p: &Path) -> zerodb_tape::Result<FileId> {
            self.hit(self.disk.open(p))
        }
        fn close(&self, f: FileId) -> zerodb_tape::Result<()> {
            self.disk.close(f)
        }
        fn write_at(&self, f: FileId, o: u64, d: &[u8]) -> zerodb_tape::Result<()> {
            self.hit(self.disk.write_at(f, o, d))
        }
        fn read_at(&self, f: FileId, o: u64, d: &mut [u8]) -> zerodb_tape::Result<usize> {
            self.hit(self.disk.read_at(f, o, d))
        }
        fn len(&self, f: FileId) -> zerodb_tape::Result<u64> {
            self.hit(self.disk.len(f))
        }
        fn set_len(&self, f: FileId, n: u64) -> zerodb_tape::Result<()> {
            self.hit(self.disk.set_len(f, n))
        }
        fn sync_data(&self, f: FileId) -> zerodb_tape::Result<()> {
            self.hit(self.disk.sync_data(f))
        }
        fn sync_dir(&self, p: &Path) -> zerodb_tape::Result<()> {
            self.hit(self.disk.sync_dir(p))
        }
        fn rename(&self, a: &Path, b: &Path) -> zerodb_tape::Result<()> {
            self.hit(self.disk.rename(a, b))
        }
        fn remove(&self, p: &Path) -> zerodb_tape::Result<()> {
            self.hit(self.disk.remove(p))
        }
        fn list(&self, p: &Path) -> zerodb_tape::Result<Vec<PathBuf>> {
            self.hit(self.disk.list(p))
        }
        fn exists(&self, p: &Path) -> zerodb_tape::Result<bool> {
            self.hit(self.disk.exists(p))
        }
        fn try_lock_exclusive(&self, p: &Path) -> zerodb_tape::Result<LockGuard> {
            self.hit(self.disk.try_lock_exclusive(p))
        }
    }
    #[test]
    fn crash_at_each_checkpoint_install_operation_keeps_a_valid_chain() {
        for seed in 0..16 {
            for at in 1..=20 {
                let disk = SimDisk::new(seed);
                let shared = state();
                append(&shared, 1);
                save(&disk, Path::new("db"), &shared).unwrap();
                append(&shared, 2);
                let io =
                    FaultIo { disk, at, count: AtomicUsize::new(0), crashed: Mutex::new(None) };
                let _ = save(&io, Path::new("db"), &shared);
                if let Some(crashed) = io.crashed.lock().unwrap().as_ref() {
                    let loaded = load(crashed, Path::new("db"), &EngineOptions::default());
                    assert!(loaded.watermark == 2 || loaded.watermark == 3, "seed {seed}, op {at}");
                    assert_eq!(loaded.warnings, 0, "seed {seed}, op {at}");
                };
            }
        }
    }
    #[test]
    fn checkpoint_delta_io_does_not_rewrite_unchanged_sessions() {
        let disk = SimDisk::new(3);
        let shared = state();
        {
            let mut state = shared.lock().unwrap();
            for i in 0..100 {
                let key = Key(1, "a".into(), i.to_string());
                state.index.reserve(&key, 1).unwrap();
                state.index.publish(&key, 1, i + 2).unwrap();
                state.dirty.insert((key, 0));
            }
            state.published_lsn = 101;
            state.txn = 100;
        }
        save(&disk, Path::new("db"), &shared).unwrap();
        {
            let mut state = shared.lock().unwrap();
            let key = Key(1, "a".into(), "0".into());
            state.index.reserve(&key, 1).unwrap();
            state.index.publish(&key, 2, 102).unwrap();
            state.dirty.insert((key, 0));
            state.published_lsn = 102;
            state.txn = 101;
        }
        save(&disk, Path::new("db"), &shared).unwrap();
        let file = disk.open(Path::new("db/ckpt-102.zdb")).unwrap();
        assert!(disk.len(file).unwrap() < 128);
        disk.close(file).unwrap();
    }
}
