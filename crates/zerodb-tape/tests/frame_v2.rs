#![allow(clippy::unwrap_used)]

use zerodb_tape::sim::frame::{self, DecodeError, Footer, Frame, Header, Kind};

#[test]
fn v2_header_is_small_and_bound_to_its_location() {
    assert_eq!(frame::HEADER_LEN, 40);
    assert_eq!(frame::VERSION, 2);
    let frame = Frame {
        header: Header {
            kind: Kind::Turn,
            version: 2,
            flags: 0,
            txn: 1,
            lsn: 1,
            commit_time_ns: 7,
            tenant_id: 1,
        },
        payload: b"hello".to_vec(),
    };
    let bytes = frame::encode(&frame, 3, 128).unwrap();
    assert_eq!(bytes.len(), 45);
    assert_eq!(frame::decode(&bytes, 3, 128).unwrap().0, frame);
    assert_eq!(frame::decode(&bytes, 4, 128), Err(DecodeError::BadCrc));
    assert_eq!(frame::decode(&bytes, 3, 129), Err(DecodeError::BadCrc));
}

#[test]
fn footer_v2_has_exact_fields_and_length() {
    let footer =
        Footer { txn: 7, first_lsn: 11, synced_lsn: 10, record_count: 3, records_crc: 0x12345678 };
    let payload = footer.encode();
    assert_eq!(payload.len(), 32);
    assert_eq!(&payload[..8], &7u64.to_le_bytes());
    assert_eq!(&payload[8..16], &11u64.to_le_bytes());
    assert_eq!(&payload[16..24], &10u64.to_le_bytes());
    assert_eq!(&payload[24..28], &3u32.to_le_bytes());
    assert_eq!(&payload[28..], &0x12345678u32.to_le_bytes());
    assert_eq!(Footer::decode(&payload).unwrap(), footer);
}

#[test]
fn real_v1_crc_reports_an_unsupported_format() {
    let mut bytes = vec![0; 120];
    bytes[4..8].copy_from_slice(&120u32.to_le_bytes());
    bytes[8] = 1;
    bytes[9] = 1;
    let crc = zerodb_tape::sim::crc32c(&bytes[4..]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(frame::decode(&bytes, 1, 0), Err(DecodeError::UnknownVersion));
}

#[test]
fn commit_time_survives_a_restart_with_a_backwards_clock() {
    use std::sync::Arc;
    use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
    use zerodb_tape::{Durability, RecordInput, SimDisk, TapeOptions};
    let disk = Arc::new(SimDisk::new(1));
    let opts = TapeOptions { segment_size: 1024, ..TapeOptions::default() };
    let mut previous = 0;
    for clock in [1000, 1, 0] {
        let (mut core, _) =
            WriterCore::open(disk.clone(), Arc::new(SimClock::new(clock)), "db", opts.clone())
                .unwrap();
        let batch = PreparedBatch::new(
            vec![RecordInput { kind: Kind::Turn, tenant_id: 1, content: vec![] }],
            Durability::Durable,
        )
        .unwrap();
        let (request, reply) = WriteRequest::new(batch);
        core.enqueue(request);
        core.step();
        reply.recv().unwrap().unwrap();
        let time = core.read(core.durable_lsn()).unwrap().unwrap().header.commit_time_ns;
        assert!(time > previous);
        previous = time;
    }
}
