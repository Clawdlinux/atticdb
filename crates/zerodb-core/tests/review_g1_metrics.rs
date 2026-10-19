#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;
use zerodb_core::{Engine, EngineOptions, NewTurn, Role, Scope};
use zerodb_tape::sim::SimClock;
use zerodb_tape::{SimDisk, TapeOptions};

#[test]
fn engine_metrics_expose_eviction_without_changing_the_index_budget() {
    let opts = EngineOptions {
        tape: TapeOptions {
            segment_size: 8192,
            lsn_index_bytes: 12 * 1024,
            ..TapeOptions::default()
        },
        checkpoint_bytes: u64::MAX,
        checkpoint_interval: Duration::from_secs(3600),
        ..EngineOptions::default()
    };
    let engine =
        Engine::open_with(Arc::new(SimDisk::new(78)), Arc::new(SimClock::default()), "db", opts)
            .unwrap();
    assert_eq!(engine.metrics().unwrap().tape_index.page_evictions, 0);
    let scope = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    for _ in 0..20 {
        engine
            .append(
                &scope,
                vec![NewTurn { role: Role::User, content: vec![7; 8], meta: vec![] }; 64],
                None,
            )
            .unwrap();
    }
    let index = engine.metrics().unwrap().tape_index;
    assert!(index.page_evictions > 0);
    assert!(index.dirty_page_evictions > 0);
    assert_eq!(index.budget_bytes, 12 * 1024);
    assert!(index.resident_bytes <= index.budget_bytes);
}
