use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zerodb_tape::sim::frame::{self, Footer, Frame, Header, Kind, VERSION};
use zerodb_tape::sim::{make_spare, DEFAULT_SEGMENT_SIZE};
use zerodb_tape::{Io, RecordInput, StdIo};

mod fs {
    const ROOT: &str = env!("CARGO_TARGET_TMPDIR");
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/support.rs"));
}
pub(crate) use fs::TestDir as BenchDir;

// Preframe fixed metadata, as in M1. Match headers, payloads, and transaction footers.
#[track_caller]
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
    /// Zero-fills later segments for `bytes` of writes before timing, as the tape's spares are.
    #[track_caller]
    pub fn new(dir: &Path, bytes: u64) -> Self {
        let io: Arc<dyn Io> = Arc::new(StdIo::new());
        for id in 1..=bytes / DEFAULT_SEGMENT_SIZE + 1 {
            make_spare(io.clone(), dir, DEFAULT_SEGMENT_SIZE, 0).unwrap();
            io.rename(&dir.join("spare.zdb"), &Self::path(dir, id)).unwrap();
        }
        io.sync_dir(dir).unwrap();
        Self {
            file: Self::segment(dir, 0),
            dir: dir.to_owned(),
            segment: 0,
            offset: 0,
            dirty: false,
        }
    }
    pub fn path(dir: &Path, id: u64) -> PathBuf {
        dir.join(format!("bare-{id}"))
    }
    /// Current segment and end offset. Bytes below the offset are durable after `sync`.
    #[allow(dead_code, reason = "only G1 reads the bare log back")]
    pub fn position(&self) -> (u64, u64) {
        (self.segment, self.offset)
    }
    #[track_caller]
    fn segment(dir: &Path, id: u64) -> File {
        let path = Self::path(dir, id);
        // Like the tape's first segment, and a roll with no spare ready.
        if !path.exists() {
            let io = StdIo::new();
            let file = io.create(&path).unwrap();
            io.allocate(file, DEFAULT_SEGMENT_SIZE).unwrap();
            io.sync_data(file).unwrap();
            io.close(file).unwrap();
            io.sync_dir(dir).unwrap();
        }
        OpenOptions::new().write(true).open(path).unwrap()
    }
    #[track_caller]
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
    #[track_caller]
    pub fn sync(&mut self) {
        if self.dirty {
            self.file.sync_data().unwrap();
            self.dirty = false;
        }
    }
}
