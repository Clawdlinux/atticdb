use crate::support::facts::new_fact;
use crate::support::sessions::{open, options};
use std::sync::{Arc, Barrier};
use zerodb_core::{Engine, EngineError, EngineOptions, ErrorKind, FactWritten, Outcome};
use zerodb_tape::sim::SimClock;
use zerodb_tape::SimDisk;

fn durable(engine: &Engine) -> u64 {
    engine.metrics().unwrap().durable_lsn
}

type Written = Result<FactWritten, EngineError>;

#[track_caller]
fn assert_rejected(engine: &Engine, write: impl FnOnce() -> Written, kind: &str, case: &str) {
    let before = durable(engine);
    let error = write().unwrap_err();
    assert_eq!(format!("{:?}", error.kind), kind, "{case}");
    assert_eq!(error.outcome, Outcome::NotCommitted, "{case}");
    assert_eq!(durable(engine), before, "{case}: nothing is written");
}

#[track_caller]
fn assert_cached(original: &FactWritten, retried: FactWritten, case: &str) {
    assert!(retried.deduplicated && !original.deduplicated, "{case}");
    assert_eq!(FactWritten { deduplicated: false, ..retried }, *original, "{case}");
}

#[test]
fn put_supersede_delete_condition_and_reopen() {
    let disk = Arc::new(SimDisk::new(60));
    let opts = EngineOptions { max_facts: 4, fact_history: 3, ..options() };
    let engine = open(disk.clone(), opts.clone());
    let first = engine.fact_put("a", new_fact("a/x", 1, "a"), Some("p1"), None).unwrap();
    assert_eq!(first.lsn, 2, "the receipt skips the tenant record");
    let second = engine.fact_put("a", new_fact("a/x", 2, "b"), Some("p2"), None).unwrap();
    assert!(second.lsn > first.lsn && !second.deduplicated);

    enum If {
        Stale,
        Current,
        Zero,
        Newest,
    }
    let (mut current, mut newest) = (second.lsn, second.lsn);
    for (case, delete, condition, commits) in [
        ("stale if_lsn conflicts", false, If::Stale, false),
        ("current if_lsn commits", false, If::Current, true),
        ("0 on a live key conflicts", false, If::Zero, false),
        ("conditional delete commits", true, If::Current, true),
        ("a tombstone's lsn is not current", false, If::Newest, false),
        ("0 on a deleted key commits", false, If::Zero, true),
    ] {
        let if_lsn = Some(match condition {
            If::Stale => first.lsn,
            If::Current => current,
            If::Zero => 0,
            If::Newest => newest,
        });
        let before = durable(&engine);
        let write = || {
            if delete {
                engine.fact_delete("a", "a/x", "a", None, if_lsn)
            } else {
                engine.fact_put("a", new_fact("a/x", 3, "a"), None, if_lsn)
            }
        };
        if commits {
            let receipt = write().unwrap();
            assert!(receipt.lsn > before && !receipt.deduplicated, "{case}");
            newest = receipt.lsn;
            current = if delete { 0 } else { receipt.lsn };
        } else {
            assert_rejected(&engine, write, "Conflict", case);
        }
    }

    let retry = engine.fact_put("a", new_fact("a/x", 1, "a"), Some("p1"), None).unwrap();
    assert_cached(&first, retry, "same key and payload");
    assert_rejected(
        &engine,
        || engine.fact_put("a", new_fact("a/x", 9, "a"), Some("p1"), None),
        "IdempotencyConflict",
        "same key, changed content",
    );
    let retry = engine.fact_put("a", new_fact("a/x", 1, "a"), Some("p1"), Some(12345)).unwrap();
    assert_cached(&first, retry, "a different if_lsn is not part of the retry identity");

    let other = engine.fact_put("b", new_fact("a/x", 1, "a"), Some("p1"), Some(0)).unwrap();
    assert!(!other.deduplicated, "tenant b has its own key and retry scope");
    let own = engine.fact_put("a", new_fact("a/x", 4, "a"), None, Some(current)).unwrap();
    current = own.lsn;
    assert_eq!(engine.metrics().unwrap().facts, 2);

    let a1 = engine.fact_put("a", new_fact("a/1", 1, "a"), Some("a1"), None).unwrap();
    engine.fact_put("a", new_fact("a/2", 1, "a"), None, None).unwrap();
    assert_eq!(engine.metrics().unwrap().facts, 4);
    assert_rejected(
        &engine,
        || engine.fact_put("a", new_fact("a/3", 1, "a"), None, None),
        "ResourceExhausted(\"facts\")",
        "a fifth new key",
    );
    assert_rejected(
        &engine,
        || engine.fact_delete("a", "a/9", "a", None, None),
        "ResourceExhausted(\"facts\")",
        "a tombstone takes a slot",
    );
    let a1 = engine.fact_put("a", new_fact("a/1", 2, "a"), None, Some(a1.lsn)).unwrap();
    engine.fact_delete("a", "a/2", "a", None, None).unwrap();
    assert_eq!(engine.metrics().unwrap().facts, 4, "updates and deletes at the cap commit");

    engine.checkpoint().unwrap();
    let last = engine.fact_put("a", new_fact("a/x", 5, "a"), Some("last"), Some(current)).unwrap();
    drop(engine);

    let engine = open(disk.clone(), opts.clone());
    assert_eq!(engine.metrics().unwrap().replayed_records, 1);
    assert_eq!(engine.metrics().unwrap().facts, 4);
    let retry = engine.fact_put("a", new_fact("a/x", 1, "a"), Some("p1"), None).unwrap();
    assert_cached(&first, retry, "checkpointed retry entry");
    let retry = engine.fact_put("a", new_fact("a/x", 5, "a"), Some("last"), None).unwrap();
    assert_cached(&last, retry, "replayed retry entry");
    assert_rejected(
        &engine,
        || engine.fact_put("a", new_fact("a/x", 6, "a"), None, Some(current)),
        "Conflict",
        "replayed current lsn",
    );
    engine.fact_put("a", new_fact("a/x", 6, "a"), None, Some(last.lsn)).unwrap();
    engine.fact_put("a", new_fact("a/1", 3, "a"), None, Some(a1.lsn)).unwrap();
    engine.fact_put("a", new_fact("a/2", 1, "a"), None, Some(0)).unwrap();
    assert_rejected(
        &engine,
        || engine.fact_put("a", new_fact("a/3", 1, "a"), None, None),
        "ResourceExhausted(\"facts\")",
        "the cap after reopen",
    );
    drop(engine);

    let lowered = EngineOptions { max_facts: 3, ..opts.clone() };
    let error =
        Engine::open_with(disk.clone(), Arc::new(SimClock::default()), "db", lowered).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::ResourceExhausted("facts")), "{error}");
    let engine = open(disk, opts);
    assert_eq!(engine.metrics().unwrap().facts, 4);
    assert_eq!(engine.metrics().unwrap().checkpoint_warnings, 0);
}

#[test]
fn changed_fact_history_on_reopen_matches_full_replay() {
    let disk = Arc::new(SimDisk::new(62));
    let with = |fact_history| EngineOptions { fact_history, ..options() };
    let engine = open(disk.clone(), with(3));
    for value in 0..6 {
        engine.fact_put("t", new_fact("k", value, "a"), None, None).unwrap();
    }
    engine.checkpoint().unwrap();
    drop(engine);
    // Full replay keeps min(revisions written, fact_history + 1).
    for (case, history, warnings, revisions, writes) in [
        ("decrease trims on load", 2, 0, 3, 1),
        ("decrease then increase replays", 3, 1, 4, 0),
        ("increase replays", 5, 1, 6, 0),
    ] {
        let engine = open(disk.clone(), with(history));
        assert_eq!(engine.metrics().unwrap().checkpoint_warnings, warnings, "{case}");
        assert_eq!(engine.metrics().unwrap().fact_revisions, revisions, "{case}");
        for value in 0..writes {
            engine.fact_put("t", new_fact("k", value, "a"), None, None).unwrap();
        }
        engine.checkpoint().unwrap();
    }
}

#[test]
fn concurrent_groups_pick_one_conditional_winner_and_bound_revisions() {
    let opts = EngineOptions { fact_history: 1, ..options() };
    let engine = open(Arc::new(SimDisk::new(61)), opts);
    let mut current = engine.fact_put("t", new_fact("k", 0, "a"), None, None).unwrap().lsn;
    let barrier = Barrier::new(8);
    for round in 0..50 {
        let before = durable(&engine);
        let results: Vec<_> = std::thread::scope(|threads| {
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let (engine, barrier) = (&engine, &barrier);
                    threads.spawn(move || {
                        barrier.wait();
                        engine.fact_put("t", new_fact("burst", i, "a"), None, None).unwrap();
                        engine.fact_put("t", new_fact("k", i, "a"), None, Some(current))
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect()
        });
        let winners: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(winners.len(), 1, "round {round}: exactly one commit");
        for error in results.iter().filter_map(|r| r.as_ref().err()) {
            assert!(matches!(error.kind, ErrorKind::Conflict), "round {round}: {error}");
            assert_eq!(error.outcome, Outcome::NotCommitted, "round {round}");
        }
        current = winners[0].lsn;
        assert_eq!(durable(&engine), before + 9, "round {round}: losers write nothing");
        // Completed same-key groups leave at most fact_history + 2 pointers per key.
        let metrics = engine.metrics().unwrap();
        assert!(metrics.fact_revisions <= metrics.facts * 3, "round {round}: {metrics:?}");
    }
}
