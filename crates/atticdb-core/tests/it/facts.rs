use crate::support::{new_fact, open, options, query, turn};
use atticdb_core::{
    forget::Selector, Engine, EngineError, EngineOptions, ErrorKind, ExportEntry, ExportQuery,
    FactHistory, FactPage, FactWritten, NewFact, Outcome, Revision, Scope, StoredFact,
};
use atticdb_tape::sim::SimClock;
use atticdb_tape::{Io, SimDisk};
use std::path::Path;
use std::sync::{Arc, Barrier};

fn durable(engine: &Engine) -> u64 {
    engine.metrics().unwrap().durable_lsn
}

fn reads(engine: &Engine) -> u64 {
    engine.metrics().unwrap().fact_reads
}

/// The current value as (content, agent, lsn).
fn value(engine: &Engine, tenant: &str, key: &str) -> Option<(u64, String, u64)> {
    let fact = engine.fact_get(tenant, key).unwrap()?;
    Some((u64::from_le_bytes(fact.content.try_into().unwrap()), fact.agent, fact.lsn))
}

/// (lsn, deleted) newest first, and `truncated`.
fn history(engine: &Engine, tenant: &str, key: &str) -> (Vec<(u64, bool)>, bool) {
    let history = engine.fact_history(tenant, key, usize::MAX).unwrap();
    (history.revisions.iter().map(|rev| (rev.lsn, rev.deleted)).collect(), history.truncated)
}

/// Every read a reopen must preserve.
fn reads_of(engine: &Engine) -> (Vec<Option<StoredFact>>, Vec<FactHistory>, FactPage) {
    let keys = [("a", "a/x"), ("a", "a/1"), ("a", "a/2"), ("b", "a/x")];
    (
        keys.iter().map(|(tenant, key)| engine.fact_get(tenant, key).unwrap()).collect(),
        keys.iter().map(|(t, key)| engine.fact_history(t, key, usize::MAX).unwrap()).collect(),
        engine.fact_list("a", query("a/", 100, usize::MAX)).unwrap(),
    )
}

/// Walks a prefix with `after`. Returns each page's keys and the tape reads it cost.
fn walk(engine: &Engine, prefix: &str, limit: usize, max_bytes: usize) -> Vec<(Vec<String>, u64)> {
    let (mut pages, mut after) = (Vec::new(), None);
    loop {
        let before = reads(engine);
        let page = engine
            .fact_list("l", atticdb_core::FactQuery { after, ..query(prefix, limit, max_bytes) });
        let page = page.unwrap();
        pages.push((page.facts.into_iter().map(|f| f.key).collect(), reads(engine) - before));
        match page.next {
            Some(next) => after = Some(next),
            None => return pages,
        }
    }
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
    assert_eq!(value(&engine, "a", "a/x"), Some((1, "a".into(), first.lsn)));
    let second = engine.fact_put("a", new_fact("a/x", 2, "b"), Some("p2"), None).unwrap();
    assert!(second.lsn > first.lsn && !second.deduplicated);
    let mut expected = Some((2, "b".to_owned(), second.lsn));
    assert_eq!(value(&engine, "a", "a/x"), expected, "a newer agent supersedes");

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
            expected = (!delete).then(|| (3, "a".into(), receipt.lsn));
        } else {
            assert_rejected(&engine, write, "Conflict", case);
        }
        assert_eq!(value(&engine, "a", "a/x"), expected, "{case}: never an older value");
    }
    let before = reads(&engine);
    let shown = vec![(current, false), (newest - 1, true), (newest - 2, false)];
    assert_eq!(history(&engine, "a", "a/x"), (shown, true), "five writes, three shown");
    assert_eq!(reads(&engine) - before, 3);
    // An export at the first write's LSN needs a revision the index trimmed.
    for (case, at, expected) in [
        ("trimmed", Some(first.lsn), Err("Conflict".to_owned())),
        ("current", None, Ok(vec![("a/x".to_owned(), current)])),
    ] {
        let query = ExportQuery { at, after: None, max_bytes: usize::MAX, max_items: 10 };
        let page = engine.export("a", query).map_err(|error| format!("{:?}", error.kind));
        let facts = page.map(|page| {
            let facts = page.entries.into_iter().map(|entry| match entry {
                ExportEntry::Fact(fact) => (fact.key, fact.lsn),
                ExportEntry::Turn { .. } => panic!("{case}: no turns"),
            });
            facts.collect::<Vec<_>>()
        });
        assert_eq!(facts, expected, "export {case}");
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
    engine.fact_put("a", new_fact("a/1", 2, "a"), None, Some(a1.lsn)).unwrap();
    let a1 = engine.fact_put("a", new_fact("a/1", 3, "a"), None, None).unwrap();
    engine.fact_delete("a", "a/2", "a", None, None).unwrap();
    assert_eq!(engine.metrics().unwrap().facts, 4, "updates and deletes at the cap commit");
    assert_eq!(value(&engine, "a", "a/2"), None);
    assert_eq!(value(&engine, "b", "a/x"), Some((1, "a".into(), other.lsn)), "tenant b's own");
    assert_eq!(history(&engine, "a", "a/1").0.len(), 3);
    assert!(!history(&engine, "a", "a/1").1, "exactly fact_history revisions");
    let before = reads(&engine);
    let cut = engine.fact_history("a", "a/1", 8).unwrap();
    assert!(cut.truncated && cut.revisions.len() == 1, "a byte budget cuts history");
    assert_eq!(reads(&engine) - before, 2, "returned revisions plus the cut read");

    engine.checkpoint().unwrap();
    let last = engine.fact_put("a", new_fact("a/2", 5, "a"), Some("last"), Some(0)).unwrap();
    let oracle = reads_of(&engine);
    drop(engine);

    let engine = open(disk.clone(), opts.clone());
    assert_eq!(engine.metrics().unwrap().replayed_records, 1);
    assert_eq!(engine.metrics().unwrap().facts, 4);
    assert_eq!(reads_of(&engine), oracle, "tag 4 keeps truncated; every read survives reopen");
    let retry = engine.fact_put("a", new_fact("a/x", 1, "a"), Some("p1"), None).unwrap();
    assert_cached(&first, retry, "checkpointed retry entry");
    let retry = engine.fact_put("a", new_fact("a/2", 5, "a"), Some("last"), None).unwrap();
    assert_cached(&last, retry, "replayed retry entry");
    assert_rejected(
        &engine,
        || engine.fact_put("a", new_fact("a/2", 6, "a"), None, Some(0)),
        "Conflict",
        "replayed current lsn",
    );
    engine.fact_put("a", new_fact("a/2", 6, "a"), None, Some(last.lsn)).unwrap();
    engine.fact_put("a", new_fact("a/x", 6, "a"), None, Some(current)).unwrap();
    engine.fact_put("a", new_fact("a/1", 4, "a"), None, Some(a1.lsn)).unwrap();
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
    drop(engine);

    // Forget hides revisions up to its cutoff. A forgotten current value leaves none (I9).
    let disk = Arc::new(SimDisk::new(64));
    let engine = open(disk.clone(), options());
    let k1 = engine.fact_put("f", new_fact("k", 1, "a"), None, None).unwrap();
    let k2 = engine.fact_put("f", new_fact("k", 2, "a"), None, None).unwrap();
    let j1 = engine.fact_put("f", new_fact("j", 1, "a"), None, None).unwrap();
    let forget = |selector, up_to| engine.forget("f", selector, None, up_to).unwrap();
    let key = |key: &str| Selector::FactKey(key.into());
    let shown = |engine: &Engine, key: &str| {
        let history = engine.fact_history("f", key, usize::MAX).unwrap();
        history.revisions.iter().map(|rev| (rev.lsn, rev.redacted)).collect::<Vec<_>>()
    };
    let counts = |engine: &Engine| {
        let m = engine.metrics().unwrap();
        (m.facts, m.fact_revisions, m.compressed_turns)
    };
    let held = counts(&engine);
    forget(key("k"), Some(k1.lsn));
    assert_eq!(value(&engine, "f", "k"), Some((2, "a".into(), k2.lsn)), "up_to below current");
    assert_eq!(shown(&engine, "k"), [(k2.lsn, false), (k1.lsn, true)]);
    let before = reads(&engine);
    let current = forget(key("k"), None);
    assert_eq!(current.cutoff, current.lsn);
    assert_eq!(value(&engine, "f", "k"), None, "never an older value");
    assert_eq!(shown(&engine, "k"), [(k2.lsn, true), (k1.lsn, true)]);
    let listed = engine.fact_list("f", query("", 100, usize::MAX)).unwrap();
    assert_eq!(listed.facts.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), ["j"]);
    assert_eq!(reads(&engine) - before, 1, "placeholders take no tape read");
    assert_eq!(counts(&engine), held, "facts, fact_revisions and compressed_turns are visible");
    assert_eq!(engine.metrics().unwrap().forgotten_fact_keys, 1);
    assert_rejected(
        &engine,
        || engine.fact_put("f", new_fact("k", 3, "a"), None, Some(k2.lsn)),
        "Conflict",
        "a forgotten revision is not current",
    );
    let k3 = engine.fact_put("f", new_fact("k", 3, "a"), None, Some(0)).unwrap();
    assert_eq!(value(&engine, "f", "k"), Some((3, "a".into(), k3.lsn)), "a later put is current");
    // An LSN aimed at a forget record undoes nothing. `up_to_lsn` above the forget is clamped.
    let clamped = forget(Selector::Lsns(vec![current.lsn, j1.lsn]), Some(u64::MAX));
    assert_eq!(clamped.cutoff, clamped.lsn);
    assert_eq!(value(&engine, "f", "k"), Some((3, "a".into(), k3.lsn)));
    assert_eq!(value(&engine, "f", "j"), None);
    let oracle = |engine: &Engine| {
        let m = engine.metrics().unwrap();
        let forgotten = (m.forgotten_sessions, m.forgotten_fact_keys, m.forgotten_lsns);
        let listed = engine.fact_list("f", query("", 100, usize::MAX)).unwrap();
        (value(engine, "f", "k"), shown(engine, "k"), shown(engine, "j"), listed, forgotten)
    };
    let expected = oracle(&engine);
    assert_eq!(expected.4, (0, 1, 2));
    let held = counts(&engine);
    drop(engine);
    let engine = open(disk, options());
    assert_eq!(oracle(&engine), expected, "reopen keeps every forget");
    assert_eq!(counts(&engine), held);
    drop(engine);

    let disk = Arc::new(SimDisk::new(63));
    let engine = open(disk.clone(), options());
    for key in ["a/1", "a/2", "a/3", "ab", "b/1"] {
        engine.fact_put("l", new_fact(key, 1, "a"), None, None).unwrap();
    }
    engine.fact_delete("l", "a/2", "a", None, None).unwrap();
    for i in 0..1025 {
        engine.fact_delete("l", &format!("t/{i:04}"), "a", None, None).unwrap();
    }
    engine.fact_put("l", new_fact("t/z", 1, "a"), None, None).unwrap();
    let keys = |keys: &[&str]| keys.iter().map(|key| key.to_string()).collect::<Vec<_>>();
    // Reads per page are returned facts, plus 1 when the byte budget cut the page.
    let rows = [
        ("prefix a/", "a/", 100, usize::MAX, vec![(keys(&["a/1", "a/3"]), 2)]),
        ("prefix a", "a", 100, usize::MAX, vec![(keys(&["a/1", "a/3", "ab"]), 3)]),
        (
            "limit 1 walks every live key once",
            "a",
            1,
            usize::MAX,
            vec![(keys(&["a/1"]), 1), (keys(&["a/3"]), 1), (keys(&["ab"]), 1)],
        ),
        (
            "a budget below the first fact",
            "a/",
            100,
            1,
            vec![(keys(&["a/1"]), 2), (keys(&["a/3"]), 1)],
        ),
        ("visit budget", "t/", 100, usize::MAX, vec![(vec![], 0), (keys(&["t/z"]), 1)]),
    ];
    for (case, prefix, limit, max_bytes, pages) in &rows {
        assert_eq!(walk(&engine, prefix, *limit, *max_bytes), *pages, "{case}");
    }
    drop(engine);
    let engine = open(disk, options());
    for (case, prefix, limit, max_bytes, pages) in &rows {
        assert_eq!(walk(&engine, prefix, *limit, *max_bytes), *pages, "{case} after reopen");
    }
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
        let image = Arc::new(disk.crash().unwrap());
        let manifest = Path::new("db/CHECKPOINT");
        if image.exists(manifest).unwrap() {
            image.remove(manifest).unwrap();
        }
        let replayed = open(image, with(history)).fact_history("t", "k", usize::MAX).unwrap();
        let shown = engine.fact_history("t", "k", usize::MAX).unwrap();
        assert_eq!(shown, replayed, "{case}: history and truncated match full replay");
        assert!(shown.truncated && shown.revisions.len() == history, "{case}");
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

/// A fact for key `k` whose content is `v{i}`, space-padded to `width` bytes.
fn padded(i: usize, width: usize) -> NewFact {
    let content = format!("{:<width$}", format!("v{i}")).into_bytes();
    NewFact { key: "k".into(), content, meta: vec![], agent: "a".into() }
}

/// Each revision as (lsn, trimmed content).
fn revisions(history: &FactHistory) -> Vec<(u64, String)> {
    let text = |rev: &Revision| String::from_utf8_lossy(&rev.content).trim_end().to_owned();
    history.revisions.iter().map(|rev| (rev.lsn, text(rev))).collect()
}

/// Puts `k` 20 times, calling `between` before each put. Returns the LSNs, oldest first.
fn twenty(engine: &Engine, width: usize, mut between: impl FnMut(usize)) -> Vec<u64> {
    (0..20)
        .map(|i| {
            between(i);
            engine.fact_put("t", padded(i, width), None, None).unwrap().lsn
        })
        .collect()
}

#[test]
fn history_pages_past_the_index_cap() {
    let engine = open(Arc::new(SimDisk::new(70)), options());
    let s = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    let lsns = twenty(&engine, 2, |i| {
        engine.append(&s, vec![turn(i as u64)], None).unwrap();
        engine.fact_put("t", new_fact("other", i as u64, "a"), None, None).unwrap();
        if i < 3 {
            engine.fact_put("u", new_fact("k", i as u64, "a"), None, None).unwrap();
        }
    });
    let first = engine.fact_history("t", "k", usize::MAX).unwrap();
    let newest: Vec<_> = (4..20).rev().map(|i| (lsns[i], format!("v{i}"))).collect();
    assert_eq!(revisions(&first), newest, "the index holds the 16 newest");
    assert!(first.truncated);
    assert_eq!(first.next, Some(lsns[4]), "next is the oldest returned LSN");

    let rest = engine.fact_history_before("t", "k", lsns[4], usize::MAX, usize::MAX).unwrap();
    let oldest: Vec<_> = (0..4).rev().map(|i| (lsns[i], format!("v{i}"))).collect();
    assert_eq!(revisions(&rest), oldest, "other keys and tenant u never show");
    assert_eq!((rest.next, rest.truncated), (None, false));
    let all: Vec<_> = first.revisions.iter().chain(&rest.revisions).map(|r| r.lsn).collect();
    assert_eq!(all, lsns.iter().rev().copied().collect::<Vec<_>>(), "20 LSNs, newest first");
}

#[test]
fn history_at_the_cap_offers_no_next_page() {
    let engine = open(Arc::new(SimDisk::new(71)), options());
    for i in 0..16 {
        engine.fact_put("t", padded(i, 2), None, None).unwrap();
    }
    let history = engine.fact_history("t", "k", usize::MAX).unwrap();
    assert_eq!(history.revisions.len(), 16);
    assert_eq!((history.truncated, history.next), (false, None), "no empty page is offered");
}

#[test]
fn history_walk_respects_visits_and_forgets() {
    let engine = open(Arc::new(SimDisk::new(72)), options());
    let s = Scope { tenant: "t".into(), agent: "a".into(), session: "s".into() };
    let lsns = twenty(&engine, 2, |i| {
        let turns = (0..50).map(|j| turn((i * 50 + j) as u64)).collect();
        engine.append(&s, turns, None).unwrap();
    });
    engine.forget("t", Selector::Lsns(vec![lsns[2]]), None, None).unwrap();
    let first = engine.fact_history("t", "k", usize::MAX).unwrap();
    let rejected = engine.fact_history_before("t", "k", lsns[4], usize::MAX, 0).unwrap_err();
    assert!(matches!(rejected.kind, ErrorKind::InvalidInput), "{rejected}");

    let (mut pages, mut before, mut seen) = (Vec::new(), first.next, Vec::new());
    while let Some(lsn) = before {
        let page = engine.fact_history_before("t", "k", lsn, usize::MAX, 10).unwrap();
        assert_eq!(page.truncated, page.next.is_some());
        seen.extend(page.revisions.iter().map(|rev| (rev.lsn, rev.redacted)));
        before = page.next;
        pages.push(page);
    }
    assert!(pages.iter().any(|page| page.revisions.is_empty() && page.next.is_some()));
    let expected = [(lsns[3], false), (lsns[2], true), (lsns[1], false), (lsns[0], false)];
    assert_eq!(seen, expected, "the forgotten revision is a placeholder, not skipped");
}

#[test]
fn history_walk_budget_cut_resumes_at_the_cut_revision() {
    let engine = open(Arc::new(SimDisk::new(73)), options());
    let lsns = twenty(&engine, 100, |_| {});
    let first = engine.fact_history("t", "k", usize::MAX).unwrap();
    let before = first.next.unwrap();
    let cut = engine.fact_history_before("t", "k", before, 150, usize::MAX).unwrap();
    assert_eq!(revisions(&cut), [(lsns[3], "v3".to_owned())]);
    assert_eq!((cut.next, cut.truncated), (Some(lsns[2] + 1), true));
    let next = engine.fact_history_before("t", "k", cut.next.unwrap(), 150, usize::MAX).unwrap();
    assert_eq!(revisions(&next)[0], (lsns[2], "v2".to_owned()), "the cut revision comes first");
}
