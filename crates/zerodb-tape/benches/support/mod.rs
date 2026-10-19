use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use zerodb_tape::sim::frame::{self, Footer, Frame, Header, Kind, VERSION};
use zerodb_tape::sim::DEFAULT_SEGMENT_SIZE;
use zerodb_tape::RecordInput;

#[derive(Debug)]
pub struct BenchDir(pub PathBuf);
impl BenchDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::current_dir().unwrap().join(format!(
            "target/append-bench-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for BenchDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

// Preframe fixed metadata, as in M1. Match headers, payloads, and transaction footers.
pub fn encoded_batch(inputs: &[RecordInput]) -> Vec<u8> {
    assert!(!inputs.is_empty());
    let mut bytes = Vec::new();
    let mut crc = zerodb_tape::sim::Crc32c::new();
    let mut header = Header {
        kind: Kind::Turn,
        version: VERSION,
        flags: 0,
        txn: 1,
        lsn: 1,
        commit_time_ns: 0,
        tenant_id: 1,
    };
    for (i, input) in inputs.iter().enumerate() {
        header.kind = input.kind;
        header.tenant_id = input.tenant_id;
        header.lsn = i as u64 + 1;
        let encoded = frame::encode(
            &Frame { header: header.clone(), payload: input.content.clone() },
            1,
            bytes.len() as u64,
        )
        .unwrap();
        crc.update(&encoded[..4]);
        bytes.extend(encoded);
    }
    let footer = Footer {
        txn: 1,
        first_lsn: 1,
        synced_lsn: 0,
        record_count: inputs.len() as u32,
        records_crc: crc.finish(),
    };
    bytes.extend(
        frame::encode(
            &Frame {
                header: Header { kind: Kind::Footer, tenant_id: 0, ..header },
                payload: footer.encode().to_vec(),
            },
            1,
            bytes.len() as u64,
        )
        .unwrap(),
    );
    bytes
}

#[derive(Debug)]
pub struct Bare {
    file: File,
    dir: PathBuf,
    segment: u64,
    offset: u64,
    dirty: bool,
}
impl Bare {
    pub fn new(dir: &Path) -> Self {
        Self {
            file: Self::segment(dir, 0),
            dir: dir.to_owned(),
            segment: 0,
            offset: 0,
            dirty: false,
        }
    }
    fn segment(dir: &Path, id: u64) -> File {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dir.join(format!("bare-{id}")))
            .unwrap();
        file.set_len(DEFAULT_SEGMENT_SIZE).unwrap();
        file.sync_data().unwrap();
        File::open(dir).unwrap().sync_all().unwrap();
        file
    }
    pub fn write(&mut self, bytes: &[u8]) {
        assert!(bytes.len() as u64 <= DEFAULT_SEGMENT_SIZE);
        if self.offset + bytes.len() as u64 > DEFAULT_SEGMENT_SIZE {
            // Preserve the tape's sync-before-roll contract for pending group members.
            self.sync();
            self.segment += 1;
            self.file = Self::segment(&self.dir, self.segment);
            self.offset = 0;
        }
        self.file.write_all(bytes).unwrap();
        self.offset += bytes.len() as u64;
        self.dirty = true;
    }
    pub fn sync(&mut self) {
        if self.dirty {
            self.file.sync_data().unwrap();
            self.dirty = false;
        }
    }
}
