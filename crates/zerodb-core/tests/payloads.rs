#![allow(clippy::unwrap_used)]

use proptest::prelude::*;
use zerodb_core::{fact, forget};

proptest! {
    #[test]
    fn fact_round_trip(
        key in ".{0,40}", agent in ".{0,40}",
        content in prop::collection::vec(any::<u8>(), 0..4096),
        meta in prop::collection::vec(any::<u8>(), 0..1024),
        source_refs in prop::collection::vec(any::<u64>(), 0..30),
        idem_key in prop::option::of(".{0,30}"), tombstone in any::<bool>(),
    ) {
        let value = fact::Fact { key, agent, content, meta, source_refs, idem_key, tombstone };
        prop_assert_eq!(fact::decode(&fact::encode(&value).unwrap()).unwrap(), value);
    }

    #[test]
    fn forget_round_trip(
        agent in ".{0,40}", session in ".{0,40}", key in ".{0,40}",
        lsns in prop::collection::vec(any::<u64>(), 0..30),
        idem_key in prop::option::of(".{0,30}"),
        up_to_lsn in prop::option::of(any::<u64>()), tag in 0u8..3,
    ) {
        let selector = match tag {
            0 => forget::Selector::Session { agent, session },
            1 => forget::Selector::FactKey(key),
            _ => forget::Selector::Lsns(lsns),
        };
        let value = forget::Forget { selector, idem_key, up_to_lsn };
        prop_assert_eq!(forget::decode(&forget::encode(&value).unwrap()).unwrap(), value);
    }

    #[test]
    fn garbage_never_panics(data in prop::collection::vec(any::<u8>(), 0..8192)) {
        let _ = fact::decode(&data);
        let _ = forget::decode(&data);
    }
}

fn fact() -> fact::Fact {
    fact::Fact {
        key: String::new(),
        content: vec![],
        meta: vec![],
        agent: String::new(),
        source_refs: vec![],
        idem_key: None,
        tombstone: false,
    }
}

#[test]
fn payloads_reject_versions_truncation_tags_and_trailing_bytes() {
    let values = [
        fact::encode(&fact()).unwrap(),
        forget::encode(&forget::Forget {
            selector: forget::Selector::FactKey("key".into()),
            idem_key: Some("retry".into()),
            up_to_lsn: Some(1),
        })
        .unwrap(),
    ];
    for (index, mut data) in values.into_iter().enumerate() {
        let decode = |bytes: &[u8]| {
            if index == 0 {
                fact::decode(bytes).map(|_| ())
            } else {
                forget::decode(bytes).map(|_| ())
            }
        };
        for n in 0..data.len() {
            assert!(decode(&data[..n]).is_err());
        }
        data.push(0);
        assert!(decode(&data).is_err());
        data.pop();
        data[0] = 2;
        assert!(decode(&data).is_err());
    }
    let mut data = fact::encode(&fact()).unwrap();
    *data.last_mut().unwrap() = 2;
    assert!(fact::decode(&data).is_err());
    data[21] = 2;
    assert!(fact::decode(&data).is_err());
}

#[test]
fn fact_limits_apply_to_every_field_in_both_directions() {
    for field in 0..6 {
        let mut value = fact();
        match field {
            0 => value.key = "k".repeat(1024),
            1 => value.content = vec![0; 4 * 1024 * 1024],
            2 => value.meta = vec![0; 65536],
            3 => value.agent = "a".repeat(255),
            4 => value.source_refs = vec![1; 1024],
            _ => value.idem_key = Some("i".repeat(128)),
        }
        let mut encoded = fact::encode(&value).unwrap();
        assert_eq!(fact::decode(&encoded).unwrap(), value);
        match field {
            0 => value.key.push('k'),
            1 => value.content.push(0),
            2 => value.meta.push(0),
            3 => value.agent.push('a'),
            4 => value.source_refs.push(1),
            _ => value.idem_key.as_mut().unwrap().push('i'),
        }
        assert!(fact::encode(&value).is_err());
        let length_offset = [1, 5, 9, 13, 17, 22][field];
        encoded[length_offset..length_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(fact::decode(&encoded).is_err());
    }
}

#[test]
fn forget_limits_apply_to_every_selector_and_retry_key() {
    for selector in [
        forget::Selector::Session { agent: "a".repeat(256), session: String::new() },
        forget::Selector::Session { agent: String::new(), session: "s".repeat(256) },
        forget::Selector::FactKey("k".repeat(1025)),
        forget::Selector::Lsns(vec![1; 1025]),
    ] {
        assert!(
            forget::encode(&forget::Forget { selector, idem_key: None, up_to_lsn: None }).is_err()
        );
    }
    assert!(forget::encode(&forget::Forget {
        selector: forget::Selector::Lsns(vec![]),
        idem_key: Some("i".repeat(129)),
        up_to_lsn: None,
    })
    .is_err());
    for data in [
        vec![1, 4],
        vec![1, 1, 0xff, 0xff, 0xff, 0xff],
        vec![1, 2, 0xff, 0xff, 0xff, 0xff],
        vec![1, 3, 0xff, 0xff, 0xff, 0xff],
    ] {
        assert!(forget::decode(&data).is_err());
    }
}
