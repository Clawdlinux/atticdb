#![allow(clippy::unwrap_used)]

use proptest::prelude::*;
use zerodb_tape::sim::crc32c;
use zerodb_tape::sim::frame::{
    batch_digest, canon, chain_next, content_digest, decode, encode, DecodeError, Footer, Frame,
    Header, Kind, HEADER_LEN, MAX_FRAME_SIZE, VERSION,
};
use zerodb_tape::Error;

fn frame(kind: Kind, content: &[u8]) -> Frame {
    let salt = [7; 16];
    let mut payload = salt.to_vec();
    payload.extend_from_slice(content);
    Frame {
        header: Header {
            kind,
            version: VERSION,
            flags: 0,
            txn: 11,
            lsn: 22,
            commit_time_ns: 33,
            tenant_id: 44,
            tenant_prev_chain: if kind == Kind::TenantCreate || kind == Kind::Footer {
                [0; 32]
            } else {
                [5; 32]
            },
            content_digest: content_digest(&salt, content),
        },
        payload,
    }
}

fn refresh_crc(bytes: &mut [u8]) {
    let checksum = crc32c(&bytes[4..]);
    bytes[..4].copy_from_slice(&checksum.to_le_bytes());
}

proptest! {
    #[test]
    fn round_trip(
        kind in prop::sample::select(vec![1u8, 2, 3, 5, 6]),
        txn in any::<u64>(),
        lsn in any::<u64>(),
        time in any::<u64>(),
        tenant in any::<u32>(),
        salt in any::<[u8; 16]>(),
        prev in any::<[u8; 32]>(),
        content in prop::collection::vec(any::<u8>(), 0..8192),
    ) {
        let kind = Kind::try_from(kind).unwrap();
        let mut original = frame(kind, &content);
        original.header.txn = txn;
        original.header.lsn = lsn;
        original.header.commit_time_ns = time;
        original.header.tenant_id = tenant;
        if !matches!(kind, Kind::TenantCreate | Kind::Footer) {
            original.header.tenant_prev_chain = prev;
        }
        original.header.content_digest = content_digest(&salt, &content);
        original.payload[..16].copy_from_slice(&salt);
        if kind == Kind::Footer {
            original.payload = Footer {
                txn,
                record_count: tenant,
                batch_digest: prev,
            }.encode().to_vec();
        }
        let encoded = encode(&original).unwrap();
        let (decoded, consumed) = decode(&encoded).unwrap();
        prop_assert_eq!(decoded, original);
        prop_assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn arbitrary_bytes_never_panic(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = decode(&data);
    }
}

#[test]
fn fixed_header_layout_and_canon_are_little_endian() {
    let original = frame(Kind::Turn, b"content");
    let bytes = encode(&original).unwrap();
    assert_eq!(HEADER_LEN, 104);
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len());
    assert_eq!(&bytes[8..12], &[1, 1, 0, 0]);
    assert_eq!(&bytes[12..20], &11u64.to_le_bytes());
    assert_eq!(&bytes[20..28], &22u64.to_le_bytes());
    assert_eq!(&bytes[28..36], &33u64.to_le_bytes());
    assert_eq!(&bytes[36..40], &44u32.to_le_bytes());
    assert_eq!(&bytes[40..72], &[5; 32]);
    assert_eq!(&bytes[72..104], &original.header.content_digest);
    assert_eq!(&bytes[104..], &original.payload);
    let mut expected = vec![1, 1];
    expected.extend_from_slice(&11u64.to_le_bytes());
    expected.extend_from_slice(&22u64.to_le_bytes());
    expected.extend_from_slice(&33u64.to_le_bytes());
    expected.extend_from_slice(&44u32.to_le_bytes());
    assert_eq!(canon(&original.header).as_slice(), expected);
}

#[test]
fn every_single_bit_flip_is_detected() {
    // Include a power-of-2 length to catch corruption into an end marker.
    for content_len in [7, 8, 136] {
        let encoded = encode(&frame(Kind::Turn, &vec![3; content_len])).unwrap();
        for bit in 0..encoded.len() * 8 {
            let mut damaged = encoded.clone();
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert!(
                matches!(decode(&damaged), Err(DecodeError::BadCrc | DecodeError::BadLength)),
                "undetected bit {bit}: {:?}",
                decode(&damaged),
            );
        }
    }
}

#[test]
fn hashes_use_salt_canonical_headers_and_ordered_content_digests() {
    let original = frame(Kind::Forget, b"content");
    let digest = original.header.content_digest;
    assert_eq!(digest, *blake3::hash(&original.payload).as_bytes());
    assert_ne!(digest, content_digest(&[8; 16], b"content"));
    let mut input = original.header.tenant_prev_chain.to_vec();
    input.extend_from_slice(&canon(&original.header));
    input.extend_from_slice(&digest);
    assert_eq!(
        chain_next(&original.header.tenant_prev_chain, &original.header, &digest),
        *blake3::hash(&input).as_bytes()
    );
    let second = content_digest(&[1; 16], b"second");
    let mut digests = digest.to_vec();
    digests.extend_from_slice(&second);
    assert_eq!(batch_digest(&[digest, second]), *blake3::hash(&digests).as_bytes());
    assert_ne!(batch_digest(&[digest, second]), batch_digest(&[second, digest]));
    assert_eq!(batch_digest(&[]), *blake3::hash(b"").as_bytes());
}

#[test]
fn footer_layout_and_zero_chain_kinds() {
    let footer = Footer { txn: 11, record_count: 2, batch_digest: [9; 32] };
    let payload = footer.encode();
    assert_eq!(payload.len(), 44);
    assert_eq!(&payload[..8], &11u64.to_le_bytes());
    assert_eq!(&payload[8..12], &2u32.to_le_bytes());
    assert_eq!(&payload[12..], &[9; 32]);
    assert_eq!(Footer::decode(&payload).unwrap(), footer);
    assert_eq!(Footer::decode(&payload[..43]), Err(DecodeError::BadLength));
    let mut full = frame(Kind::Footer, b"");
    full.payload = payload.to_vec();
    assert_eq!(decode(&encode(&full).unwrap()).unwrap().0, full);
    for kind in [Kind::TenantCreate, Kind::Footer] {
        let mut full = frame(kind, b"content");
        if kind == Kind::Footer {
            full.payload = payload.to_vec();
        }
        let mut encoded = encode(&full).unwrap();
        encoded[40] = 1;
        refresh_crc(&mut encoded);
        assert_eq!(decode(&encoded), Err(DecodeError::BadLength));
    }
}

#[test]
fn boundaries_versions_and_end_marker() {
    assert_eq!(decode(&[0; 8]), Err(DecodeError::EndOfData));
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
    bytes[9] = 2;
    refresh_crc(&mut bytes);
    assert_eq!(decode(&bytes), Err(DecodeError::UnknownVersion));
    bytes[9] = VERSION;
    bytes[8] = 99;
    refresh_crc(&mut bytes);
    assert_eq!(decode(&bytes), Err(DecodeError::BadLength));
    let bytes = encode(&original).unwrap();
    assert_eq!(decode(&bytes[..bytes.len() - 1]), Err(DecodeError::BadLength));
    let mut concatenated = bytes.clone();
    concatenated.extend_from_slice(&bytes);
    assert_eq!(decode(&concatenated).unwrap(), (original, bytes.len()));
}

#[test]
fn an_empty_segment_tail_is_end_of_data() {
    assert_eq!(decode(&[]), Err(DecodeError::EndOfData));
}

#[test]
fn maximum_frame_size_is_enforced() {
    let mut original = frame(Kind::Turn, b"");
    original.payload.resize(MAX_FRAME_SIZE - HEADER_LEN, 0);
    assert_eq!(encode(&original).unwrap().len(), MAX_FRAME_SIZE);
    assert_eq!(decode(&encode(&original).unwrap()).unwrap().0, original);
    original.payload.push(0);
    assert!(matches!(encode(&original), Err(Error::BatchTooLarge)));
    let mut oversized = vec![0; HEADER_LEN];
    oversized[4..8].copy_from_slice(&((MAX_FRAME_SIZE + 1) as u32).to_le_bytes());
    assert_eq!(decode(&oversized), Err(DecodeError::BadLength));
}

#[test]
fn encoding_invalid_caller_input_returns_errors() {
    let mut oversized = frame(Kind::Turn, b"");
    oversized.payload.resize(MAX_FRAME_SIZE, 0);
    assert!(matches!(encode(&oversized), Err(Error::BatchTooLarge)));
    let mut invalid = frame(Kind::Turn, b"content");
    invalid.header.flags = 1;
    assert!(matches!(encode(&invalid), Err(Error::InvalidFrame(_))));
    invalid.header.flags = 0;
    invalid.header.version = VERSION + 1;
    assert!(matches!(encode(&invalid), Err(Error::InvalidFrame(_))));
}
