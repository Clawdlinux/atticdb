#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use zerodb_tape::{
    Clock, Committed, Digest, Durability, FileId, Frame, Header, Io, Kind, LockGuard, Outcome,
    RecordInput, RecoveryReport, SimDisk, StdIo, SystemClock, Tape, TapeOptions, WriteError,
    WriteResult,
};

#[test]
fn root_api_supports_append_read_and_custom_backends() {
    let disk = Arc::new(SimDisk::new(0));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let io: Arc<dyn Io> = disk;
    let _: StdIo = StdIo::new();
    let _: FileId = FileId(1);
    let _: Digest = [0; 32];
    let _: Option<Header> = None;
    let _: Option<Frame> = None;
    let _: Option<RecoveryReport> = None;
    let _: WriteResult =
        Err(WriteError { kind: zerodb_tape::Error::Poisoned, outcome: Outcome::NotCommitted });
    let _guard = LockGuard::new(|| {});
    let (tape, _) = Tape::open(io, clock, "db", TapeOptions::default()).unwrap();
    let record = RecordInput { tenant_id: 1, kind: Kind::Turn, content: b"public".to_vec() };
    let Committed::Durable { first_lsn, .. } =
        tape.append(vec![record], Durability::Durable).unwrap()
    else {
        panic!("expected durable acknowledgement");
    };
    assert_eq!(&tape.read(first_lsn).unwrap().unwrap().payload[16..], b"public");
}
