#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use zerodb_tape::dir::DataDir;
use zerodb_tape::frame::{self, Footer, Frame, Header, Kind, VERSION};
use zerodb_tape::io::Io;
use zerodb_tape::recovery::recover;
use zerodb_tape::simdisk::SimDisk;
use zerodb_tape::Error;

fn transaction(lsn: u64, txn: u64, prev: [u8; 32]) -> (Vec<u8>, [u8; 32]) {
    let header = Header {
        kind: Kind::Turn,
        version: VERSION,
        flags: 0,
        txn,
        lsn,
        commit_time_ns: 10,
        tenant_id: 1,
        tenant_prev_chain: prev,
        content_digest: frame::content_digest(&[0; 16], b"hello"),
    };
    let head = frame::chain_next(&prev, &header, &header.content_digest);
    let mut payload = vec![0; 16];
    payload.extend_from_slice(b"hello");
    let mut bytes = frame::encode(&Frame { header: header.clone(), payload }).unwrap();
    let footer = Footer {
        txn,
        record_count: 1,
        batch_digest: frame::batch_digest(&[header.content_digest]),
    };
    bytes.extend(
        frame::encode(&Frame {
            header: Header {
                kind: Kind::Footer,
                tenant_id: 0,
                tenant_prev_chain: [0; 32],
                content_digest: [0; 32],
                ..header
            },
            payload: footer.encode().to_vec(),
        })
        .unwrap(),
    );
    (bytes, head)
}

fn setup() -> (Arc<SimDisk>, DataDir) {
    let io = Arc::new(SimDisk::new(1));
    let mut dir = DataDir::open(io.clone(), "db", None).unwrap();
    dir.prepare_next_segment(1024).unwrap();
    (io, dir)
}

fn write(io: &SimDisk, dir: &DataDir, id: u64, offset: u64, bytes: &[u8]) {
    let file = io.open(&dir.segment_path(id)).unwrap();
    io.write_at(file, offset, bytes).unwrap();
    io.sync_data(file).unwrap();
    io.close(file).unwrap();
}

#[test]
fn clean_reopen_and_chain_heads_allow_lsn_gaps() {
    let (io, mut dir) = setup();
    let (first, head) = transaction(1, 1, [0; 32]);
    let (second, _) = transaction(8, 2, head);
    write(&io, &dir, 1, 0, &[first, second].concat());
    dir.install_superblock(8).unwrap();
    let recovered = recover(&mut dir, 1024).unwrap();
    assert_eq!(recovered.report.durable_lsn, 8);
    assert_eq!(recovered.report.records, 2);
    assert!(recovered.report.truncated.is_none());
}

#[test]
fn missing_footer_drops_whole_transaction_and_zeroes_tail() {
    let (io, mut dir) = setup();
    let (first, head) = transaction(1, 1, [0; 32]);
    let (second, _) = transaction(2, 2, head);
    write(&io, &dir, 1, 0, &first);
    write(&io, &dir, 1, first.len() as u64, &second[..second.len() - 10]);
    let recovered = recover(&mut dir, 1024).unwrap();
    assert_eq!(recovered.report.durable_lsn, 1);
    assert_eq!(recovered.report.truncated.as_ref().unwrap().0, 2..=2);
    let crash = io.crash().unwrap();
    let file = crash.open(&dir.segment_path(1)).unwrap();
    let mut tail = vec![1; 1024 - first.len()];
    crash.read_at(file, first.len() as u64, &mut tail).unwrap();
    crash.close(file).unwrap();
    assert!(tail.iter().all(|&byte| byte == 0));
}

#[test]
fn damage_missing_segment_and_early_end_below_superblock_are_corruption() {
    for failure in 0..3 {
        let (io, mut dir) = setup();
        let (bytes, _) = transaction(1, 1, [0; 32]);
        write(&io, &dir, 1, 0, &bytes);
        dir.install_superblock(1).unwrap();
        match failure {
            0 => {
                let file = io.open(&dir.segment_path(1)).unwrap();
                io.flip_bit(file, 105).unwrap();
                io.close(file).unwrap();
            }
            1 => io.remove(&dir.segment_path(1)).unwrap(),
            _ => write(&io, &dir, 1, 0, &[0; 8]),
        }
        assert!(matches!(recover(&mut dir, 1024), Err(Error::CorruptionDetected { .. })));
    }
}

#[test]
fn bad_chain_or_footer_is_not_replayed() {
    for footer in [false, true] {
        let (io, mut dir) = setup();
        let (mut bytes, _) = transaction(1, 1, if footer { [0; 32] } else { [1; 32] });
        if footer {
            let (record, used) = frame::decode(&bytes).unwrap();
            let (mut end, _) = frame::decode(&bytes[used..]).unwrap();
            end.payload[12] ^= 1;
            bytes = [frame::encode(&record).unwrap(), frame::encode(&end).unwrap()].concat();
        }

        write(&io, &dir, 1, 0, &bytes);
        let result = recover(&mut dir, 1024).unwrap();
        assert_eq!(result.report.records, 0);
        assert!(result.report.truncated.is_some());
    }
}

#[test]
fn nonincreasing_lsn_is_truncated_above_and_corruption_below_boundary() {
    for boundary in [0, 2] {
        let (io, mut dir) = setup();
        let (first, head) = transaction(1, 1, [0; 32]);
        let (second, _) = transaction(1, 2, head);
        write(&io, &dir, 1, 0, &[first, second].concat());
        dir.install_superblock(boundary).unwrap();
        let result = recover(&mut dir, 1024);
        if boundary == 0 {
            assert_eq!(result.unwrap().report.records, 1);
        } else {
            assert!(matches!(result, Err(Error::CorruptionDetected { .. })));
        }
    }
}

#[test]
fn truncation_report_includes_discarded_later_segments() {
    let (io, mut dir) = setup();
    let (first, head) = transaction(1, 1, [0; 32]);
    let (second, head) = transaction(2, 2, head);
    let (third, _) = transaction(3, 3, head);
    dir.prepare_next_segment(1024).unwrap();
    write(&io, &dir, 1, 0, &first);
    write(&io, &dir, 1, first.len() as u64, &second[..second.len() - 10]);
    write(&io, &dir, 2, 0, &third);
    let state = recover(&mut dir, 1024).unwrap();
    assert_eq!(state.report.truncated.unwrap().0, 2..=3);
}
