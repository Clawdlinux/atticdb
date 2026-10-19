#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use zerodb_tape::sim::SimClock;
use zerodb_tape::{Durability, Kind, RecordInput, SimDisk, Tape, TapeOptions};

#[test]
fn tenant_create_is_decodable_unchained_and_atomic_with_turns() {
    let disk = Arc::new(SimDisk::new(5));
    let opts = TapeOptions { segment_size: 4096, ..TapeOptions::default() };
    let (tape, _) =
        Tape::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone()).unwrap();
    for _ in 0..2 {
        tape.append(
            vec![
                RecordInput { kind: Kind::TenantCreate, tenant_id: 1, content: b"tenant".to_vec() },
                RecordInput { kind: Kind::Turn, tenant_id: 1, content: b"turn".to_vec() },
            ],
            Durability::Durable,
        )
        .unwrap();
    }
    let tenant = tape.read(3).unwrap().unwrap();
    assert_eq!(tenant.header.kind, Kind::TenantCreate);
    drop(tape);
    let (tape, _) = Tape::open(disk, Arc::new(SimClock::default()), "db", opts).unwrap();
    assert_eq!(tape.scan(0).count(), 4);
}
