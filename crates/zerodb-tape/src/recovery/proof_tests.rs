#![allow(clippy::unwrap_used)]

use super::*;

fn run(count: usize, closed: usize, bad_crc: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut crcs = Vec::new();
    for lsn in 1..=count as u64 {
        let record = Frame {
            header: frame::Header {
                kind: Kind::Turn,
                version: 2,
                flags: 0,
                txn: 2,
                lsn,
                commit_time_ns: 2,
                tenant_id: 1,
            },
            payload: vec![7; 8],
        };
        let encoded = frame::encode(&record, 1, bytes.len() as u64).unwrap();
        crcs.extend_from_slice(&encoded[..4]);
        bytes.extend(encoded);
    }
    let first = count - closed + 1;
    let footer = Footer {
        txn: 2,
        first_lsn: first as u64,
        synced_lsn: u64::from(first > 1),
        record_count: closed as u32,
        records_crc: crate::crc32c::crc32c(&crcs[(first - 1) * 4..]) ^ u32::from(bad_crc),
    };
    let encoded = frame::encode(
        &Frame {
            header: frame::Header {
                kind: Kind::Footer,
                version: 2,
                flags: 0,
                txn: 2,
                lsn: count as u64,
                commit_time_ns: 2,
                tenant_id: 0,
            },
            payload: footer.encode().to_vec(),
        },
        1,
        bytes.len() as u64,
    )
    .unwrap();
    bytes.extend(encoded);
    bytes
}

#[test]
fn valid_and_invalid_unproving_footers_do_not_rescan_every_record_suffix() {
    for bad_crc in [false, true] {
        let bytes = run(1024, 1024, bad_crc);
        let mut decoded = 0;
        assert!(!scan_proof(&bytes, 1, 0, &Recovered::default(), || decoded += 1));
        assert!(decoded <= 1024 + 128, "decoded {decoded} frames");
    }
}

#[test]
fn a_footer_can_validate_an_overlapping_suffix_after_a_false_start() {
    for count in [16, 1400] {
        let closed = (count - 1).min(1024);
        let bytes = run(count, closed, false);
        let mut decoded = 0;
        assert!(scan_proof(&bytes, 1, 0, &Recovered::default(), || decoded += 1));
        assert!(decoded <= count + 128, "decoded {decoded} frames");
    }
}
