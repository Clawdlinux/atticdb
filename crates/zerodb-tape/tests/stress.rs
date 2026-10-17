#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};
use zerodb_tape::frame::Kind;
use zerodb_tape::io::{StdIo, SystemClock};
use zerodb_tape::{Committed, Durability, Options, RecordInput, Tape};

/// Run with `cargo test --workspace --release`. Debug builds skip the filesystem stress workload.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "20,000 filesystem syncs; run cargo test --workspace --release"
)]
fn concurrent_durable_appends_survive_reopen() {
    let path =
        std::env::current_dir().unwrap().join(format!("target/stress-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let io = Arc::new(StdIo::new());
    let clock = Arc::new(SystemClock);
    let (tape, _) = Tape::open(io.clone(), clock.clone(), &path, Options::default()).unwrap();
    let barrier = Barrier::new(16);
    let acknowledged = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..16u32)
            .map(|tenant| {
                let tape = &tape;
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut rng = u64::from(tenant) + 1;
                    barrier.wait();
                    (0..1250u32)
                        .map(|sequence| {
                            rng ^= rng << 13;
                            rng ^= rng >> 7;
                            rng ^= rng << 17;
                            let mut content = vec![rng as u8; (rng % 8193) as usize];
                            let prefix = sequence.to_le_bytes();
                            let count = content.len().min(prefix.len());
                            content[..count].copy_from_slice(&prefix[..count]);
                            let record = RecordInput {
                                tenant_id: tenant,
                                kind: Kind::Turn,
                                content: content.clone(),
                            };
                            let ack = tape.append(vec![record], Durability::Durable).unwrap();
                            let Committed::Durable { first_lsn, last_lsn, .. } = ack else {
                                panic!("durable request received Fast acknowledgement");
                            };
                            assert_eq!(first_lsn, last_lsn);
                            (first_lsn, tenant, content)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        threads.into_iter().flat_map(|thread| thread.join().unwrap()).collect::<Vec<_>>()
    });
    assert_eq!(acknowledged.len(), 20_000);
    assert_eq!(tape.durable_lsn(), 20_000);
    drop(tape);
    let (tape, report) = Tape::open(io, clock, &path, Options::default()).unwrap();
    assert_eq!(report.records, 20_000);
    let mut unique = BTreeMap::new();
    for (lsn, tenant, content) in acknowledged {
        assert!(unique.insert(lsn, ()).is_none());
        let frame = tape.read(lsn).unwrap().unwrap();
        assert_eq!(frame.header.lsn, lsn);
        assert_eq!(frame.header.tenant_id, tenant);
        assert_eq!(frame.header.kind, Kind::Turn);
        assert_eq!(&frame.payload[16..], content);
    }
    drop(tape);
    std::fs::remove_dir_all(path).unwrap();
}
