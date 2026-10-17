use proptest::prelude::*;
use zerodb_tape::crc32c::{crc32c, Crc32c};

#[test]
fn known_vectors() {
    assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    assert_eq!(crc32c(b""), 0);
    assert_eq!(crc32c(&[0; 32]), 0x8A91_36AA);
}

proptest! {
    #[test]
    fn incremental_matches_one_shot(
        data in prop::collection::vec(any::<u8>(), 0..8192),
        splits in prop::collection::vec(any::<usize>(), 0..64),
    ) {
        let mut checksum = Crc32c::new();
        let mut start = 0;
        for split in splits {
            let end = start + split % (data.len() - start + 1);
            checksum.update(&data[start..end]);
            start = end;
        }
        checksum.update(&data[start..]);
        prop_assert_eq!(checksum.finish(), crc32c(&data));
    }

    #[test]
    fn slicing_matches_bitwise(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut crc = !0u32;
        for byte in &data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0x82F6_3B78 & 0u32.wrapping_sub(crc & 1));
            }
        }
        prop_assert_eq!(crc32c(&data), !crc);
    }
}
