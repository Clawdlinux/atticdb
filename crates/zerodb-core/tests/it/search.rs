use crate::support::{open, options, scope};
use std::sync::Arc;
use zerodb_core::{
    Engine, EngineOptions, ErrorKind, Hit, NewFact, NewTurn, Role, Scope, SearchQuery,
};
use zerodb_tape::SimDisk;

fn say(engine: &Engine, scope: &Scope, text: &str) {
    let turn = NewTurn { role: Role::User, content: text.into(), meta: vec![] };
    engine.append(scope, vec![turn], None).unwrap();
}

fn set(engine: &Engine, key: &str, text: &str) {
    let fact = NewFact { key: key.into(), content: text.into(), meta: vec![], agent: "a".into() };
    engine.fact_put("tenant", fact, None, None).unwrap();
}

fn q(words: &str) -> SearchQuery {
    SearchQuery {
        query: words.into(),
        before: None,
        limit: 10,
        max_bytes: 1 << 20,
        visits: 1 << 20,
    }
}

/// Every hit, following `next`, as `session:content` or `key=content`.
fn search(engine: &Engine, tenant: &str, mut query: SearchQuery) -> Vec<String> {
    let mut found = Vec::new();
    loop {
        let page = engine.search(tenant, query.clone()).unwrap();
        found.extend(page.hits.into_iter().map(|hit| match hit {
            Hit::Turn { session, turn, .. } => {
                format!("{session}:{}", String::from_utf8_lossy(&turn.content))
            }
            Hit::Fact(fact) => format!("{}={}", fact.key, String::from_utf8_lossy(&fact.content)),
        }));
        match page.next {
            Some(next) => query.before = Some(next),
            None => return found,
        }
    }
}

#[test]
fn search_finds_current_records_newest_first() {
    let engine = open(Arc::new(SimDisk::new(60)), options());
    say(&engine, &scope("s1"), "Retry rule: back off 2 s");
    say(&engine, &scope("s1"), "unrelated");
    say(&engine, &Scope { tenant: "other".into(), ..scope("s1") }, "retry secret");
    set(&engine, "repo/api/retry", "old value");
    set(&engine, "repo/api/gone", "retry gone");
    engine.fact_delete("tenant", "repo/api/gone", "a", None, None).unwrap();
    say(&engine, &scope("s2"), "RETRYING capped at 5");
    set(&engine, "repo/api/retry", "use idempotency keys");

    let all = [
        "repo/api/retry=use idempotency keys",
        "s2:RETRYING capped at 5",
        "s1:Retry rule: back off 2 s",
    ];
    for (case, query, expected) in [
        ("case-insensitive substring, newest first", q("retry"), &all[..]),
        ("every word must match", q("retry  BACK"), &all[2..]),
        ("a superseded value is gone", q("old value"), &[][..]),
        ("a deleted key is gone", q("gone"), &[][..]),
        ("facts match on content", q("idempotency"), &all[..1]),
        ("1 hit per page", SearchQuery { limit: 1, ..q("retry") }, &all[..]),
        ("the first hit always fits", SearchQuery { max_bytes: 1, ..q("retry") }, &all[..]),
        ("2 records per call", SearchQuery { visits: 2, ..q("retry") }, &all[..]),
    ] {
        assert_eq!(search(&engine, "tenant", query), expected, "{case}");
    }
    assert_eq!(search(&engine, "other", q("retry")), ["s1:retry secret"], "tenants");
    assert!(search(&engine, "nobody", q("retry")).is_empty(), "unknown tenant");
    for bad in [q("  "), SearchQuery { limit: 0, ..q("x") }, SearchQuery { visits: 0, ..q("x") }] {
        let kind = engine.search("tenant", bad).unwrap_err().kind;
        assert!(matches!(kind, ErrorKind::InvalidInput));
    }
}

#[test]
fn search_never_shows_a_value_replaced_mid_walk() {
    let engine = open(Arc::new(SimDisk::new(61)), EngineOptions { fact_history: 1, ..options() });
    set(&engine, "k", "zebra v1");
    for i in 0..4 {
        say(&engine, &scope("s"), &format!("zebra turn {i}"));
    }
    let first = engine.search("tenant", SearchQuery { visits: 2, ..q("zebra") }).unwrap();
    // 3 separate publications replace k, pruning the revision the first page saw as current.
    for v in 2..5 {
        set(&engine, "k", &format!("zebra v{v}"));
    }
    let rest = SearchQuery { visits: 2, before: first.next, ..q("zebra") };
    let mut seen: Vec<String> = search(&engine, "tenant", rest);
    seen.sort();
    assert_eq!(seen, ["s:zebra turn 0", "s:zebra turn 1"], "v1 is gone, v4 came after the walk");
    assert_eq!(search(&engine, "tenant", q("zebra"))[0], "k=zebra v4", "a new search sees v4");
}
