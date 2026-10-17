#![allow(clippy::unwrap_used)]

use proptest::prelude::*;
use zerodb_core::turn::{decode, encode, NewTurn, Role, Trust};

fn turn() -> NewTurn {
    NewTurn { role: Role::User, content: b"hello".to_vec(), meta: vec![], trust: Trust::Untrusted }
}

#[test]
fn limits_and_tags_are_checked() {
    let mut value = turn();
    assert!(encode(&"a".repeat(256), "s", 0, &value, None).is_err());
    assert!(encode("a", &"s".repeat(256), 0, &value, None).is_err());
    assert!(encode("a", "s", 0, &value, Some(&"k".repeat(129))).is_err());
    value.meta = vec![0; 65537];
    assert!(encode("a", "s", 0, &value, None).is_err());
    value.meta = vec![0; 65536];
    value.content = vec![0; 4 * 1024 * 1024 + 1];
    assert!(encode("a", "s", 0, &value, None).is_err());
    value.content.pop();
    let mut bytes =
        encode(&"a".repeat(255), &"s".repeat(255), 0, &value, Some(&"k".repeat(128))).unwrap();
    assert_eq!(decode(&bytes).unwrap().turn, value);
    bytes[0] = 99;
    assert!(decode(&bytes).is_err());
    let bytes = encode("a", "s", 4, &turn(), None).unwrap();
    for len in 0..bytes.len() {
        assert!(decode(&bytes[..len]).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(decode(&trailing).is_err());
}

proptest! {
    #[test]
    fn round_trip(
        agent in ".{0,40}", session in ".{0,40}", seq in any::<u64>(),
        content in prop::collection::vec(any::<u8>(), 0..4096),
        meta in prop::collection::vec(any::<u8>(), 0..1024),
        key in prop::option::of(".{0,30}"), role in 0u8..4, trusted in any::<bool>(),
    ) {
        let value = NewTurn {
            role: [Role::System, Role::User, Role::Assistant, Role::Tool][role as usize],
            content, meta,
            trust: if trusted { Trust::Trusted } else { Trust::Untrusted },
        };
        let bytes = encode(&agent, &session, seq, &value, key.as_deref()).unwrap();
        let decoded = decode(&bytes).unwrap();
        prop_assert_eq!(decoded.agent_id, agent);
        prop_assert_eq!(decoded.session_id, session);
        prop_assert_eq!(decoded.seq, seq);
        prop_assert_eq!(decoded.turn, value);
        prop_assert_eq!(decoded.idem_key, key);
    }

    #[test]
    fn garbage_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..8192)) {
        let _ = decode(&bytes);
    }
}
