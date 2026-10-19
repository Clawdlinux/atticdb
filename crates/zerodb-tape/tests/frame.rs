#![allow(clippy::unwrap_used)]

use proptest::prelude::*;
use zerodb_tape::sim::frame::{
    self, DecodeError, Footer, Frame, Header, Kind, HEADER_LEN, MAX_FRAME_SIZE, VERSION,
};
use zerodb_tape::Error;

fn frame(kind: Kind, content: &[u8]) -> Frame {
    Frame {
        header: Header {
            kind,
            version: VERSION,
            flags: 0,
            txn: 11,
            lsn: 22,
            commit_time_ns: 33,
            tenant_id: 44,
        },
        payload: content.to_vec(),
    }
}
fn encode(frame: &Frame) -> zerodb_tape::Result<Vec<u8>> {
    frame::encode(frame, 7, 128)
}
fn decode(bytes: &[u8]) -> Result<(Frame, usize), DecodeError> {
    frame::decode(bytes, 7, 128)
}
fn refresh_crc(bytes: &mut [u8]) {
    let crc = frame::checksum(7, 128, &bytes[4..]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
}

proptest! {
    #[test]
    fn round_trip(
        kind in prop::sample::select(vec![1u8, 2, 3, 5, 6]),
        txn in any::<u64>(), lsn in any::<u64>(), time in any::<u64>(),
        tenant in any::<u32>(), content in prop::collection::vec(any::<u8>(), 0..8192),
        segment in any::<u64>(), offset in any::<u64>(),
    ) {
        let kind = Kind::try_from(kind).unwrap();
        let mut original = frame(kind, &content);
        original.header.txn = txn;
        original.header.lsn = lsn;
        original.header.commit_time_ns = time;
        original.header.tenant_id = tenant;
        if kind == Kind::Footer {
            original.payload = Footer {
                txn, first_lsn: lsn, synced_lsn: 0, record_count: 1, records_crc: tenant,
            }.encode().to_vec();
        }
        let encoded = frame::encode(&original, segment, offset).unwrap();
        let (decoded, consumed) = frame::decode(&encoded, segment, offset).unwrap();
        prop_assert_eq!(decoded, original);
        prop_assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn arbitrary_bytes_never_panic(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = decode(&data);
    }
}

#[test]
fn fixed_header_layout_is_little_endian() {
    let original = frame(Kind::Turn, b"content");
    let bytes = encode(&original).unwrap();
    assert_eq!(HEADER_LEN, 40);
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len());
    assert_eq!(&bytes[8..12], &[1, 2, 0, 0]);
    assert_eq!(&bytes[12..20], &11u64.to_le_bytes());
    assert_eq!(&bytes[20..28], &22u64.to_le_bytes());
    assert_eq!(&bytes[28..36], &33u64.to_le_bytes());
    assert_eq!(&bytes[36..40], &44u32.to_le_bytes());
    assert_eq!(&bytes[40..], &original.payload);
}

#[test]
fn every_single_bit_flip_is_detected() {
    for content_len in [7, 8, 88] {
        let encoded = encode(&frame(Kind::Turn, &vec![3; content_len])).unwrap();
        for bit in 0..encoded.len() * 8 {
            let mut damaged = encoded.clone();
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert!(matches!(decode(&damaged), Err(DecodeError::BadCrc | DecodeError::BadLength)));
        }
    }
}

#[test]
fn boundaries_versions_and_end_marker() {
    assert_eq!(decode(&[0; 8]), Err(DecodeError::EndOfData));
    assert_eq!(decode(&[]), Err(DecodeError::EndOfData));
    for length in 1..HEADER_LEN {
        if length != 8 {
            let mut bytes = vec![0; length];
            if length >= 8 {
                bytes[4..8].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
            }
            assert_eq!(decode(&bytes), Err(DecodeError::Truncated));
        }
    }
    let original = frame(Kind::Turn, b"content");
    let mut bytes = encode(&original).unwrap();
    bytes[9] = 1;
    refresh_crc(&mut bytes);
    assert_eq!(decode(&bytes), Err(DecodeError::UnknownVersion));
    bytes[9] = VERSION;
    for kind in [0, 4, 7, 99] {
        bytes[8] = kind;
        refresh_crc(&mut bytes);
        assert_eq!(decode(&bytes), Err(DecodeError::BadLength));
    }
    let bytes = encode(&original).unwrap();
    assert_eq!(decode(&bytes[..bytes.len() - 1]), Err(DecodeError::BadLength));
    let mut concatenated = bytes.clone();
    concatenated.extend_from_slice(&bytes);
    assert_eq!(decode(&concatenated).unwrap(), (original, bytes.len()));
}

#[test]
fn maximum_frame_size_and_invalid_metadata_are_enforced() {
    let mut original = frame(Kind::Turn, b"");
    original.payload.resize(MAX_FRAME_SIZE - HEADER_LEN, 0);
    assert_eq!(encode(&original).unwrap().len(), MAX_FRAME_SIZE);
    assert_eq!(decode(&encode(&original).unwrap()).unwrap().0, original);
    original.payload.push(0);
    assert!(matches!(encode(&original), Err(Error::BatchTooLarge)));
    let mut oversized = vec![0; HEADER_LEN];
    oversized[4..8].copy_from_slice(&((MAX_FRAME_SIZE + 1) as u32).to_le_bytes());
    assert_eq!(decode(&oversized), Err(DecodeError::BadLength));
    let mut invalid = frame(Kind::Turn, b"content");
    invalid.header.flags = 1;
    assert!(matches!(encode(&invalid), Err(Error::InvalidFrame(_))));
    invalid.header.flags = 0;
    invalid.header.version = 1;
    assert!(matches!(encode(&invalid), Err(Error::InvalidFrame(_))));
    assert!(Footer::decode(&[0; 31]).is_err());
    assert!(Footer::decode(&[0; 33]).is_err());
}
