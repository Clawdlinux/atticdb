#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
use zerodb_tape::{Durability, Error, Io, Kind, RecordInput, SimDisk, Tape, TapeOptions};

fn footer_damage(misdirected: bool) {
    for size in [128, 1024] {
        let disk = Arc::new(SimDisk::new(73));
        let opts = TapeOptions { segment_size: size, ..TapeOptions::default() };
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone())
                .unwrap();
        for value in [1, 2] {
            let batch = PreparedBatch::new(
                vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![value; 8] }],
                Durability::Durable,
            )
            .unwrap();
            let (request, reply) = WriteRequest::new(batch);
            core.enqueue(request);
            core.step();
            reply.recv().unwrap().unwrap();
        }
        drop(core);
        let first = disk.open(Path::new("db/seg-0000000000000001.zdb")).unwrap();
        if misdirected {
            let (path, offset) = if size == 128 {
                ("db/seg-0000000000000002.zdb", 0)
            } else {
                ("db/seg-0000000000000001.zdb", 120)
            };
            let second = disk.open(Path::new(path)).unwrap();
            let mut frame = [0; 48];
            assert_eq!(disk.read_at(second, offset, &mut frame).unwrap(), frame.len());
            disk.close(second).unwrap();
            disk.write_at(first, 48, &frame).unwrap();
        } else {
            disk.write_at(first, 48, &[0xff; 72]).unwrap();
        }
        disk.sync_data(first).unwrap();
        disk.close(first).unwrap();
        assert!(
            matches!(
                Tape::open(disk, Arc::new(SimClock::default()), "db", opts),
                Err(Error::CorruptionDetected { .. })
            ),
            "misdirected={misdirected}, segment_size={size}"
        );
    }
}

#[test]
fn a_whole_garbage_footer_cannot_hide_the_next_synced_transaction() {
    footer_damage(false);
}

#[test]
fn a_misdirected_frame_over_a_footer_cannot_hide_the_next_synced_transaction() {
    footer_damage(true);
}
