use crate::support::{open, options, scope, turn};
use std::sync::Arc;
use zerodb_core::{
    Engine, EngineOptions, ErrorKind, Hit, NewFact, NewTurn, Outcome, Record, RecordKind,
    RecordQuery, Role, Scope, SearchQuery, SessionQuery, WindowQuery,
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

fn rq() -> RecordQuery {
    RecordQuery { before: None, limit: 50, kind: None, max_bytes: 1 << 20, visits: 1 << 20 }
}

/// `lsn turn agent/session#seq content` or `lsn fact key content`. A delete's content is
/// `deleted`.
fn line(record: &Record) -> String {
    match record {
        Record::Turn { agent, session, turn } => {
            let content = String::from_utf8_lossy(&turn.content);
            format!("{} turn {agent}/{session}#{} {content}", turn.lsn, turn.seq)
        }
        Record::Fact { key, revision } => {
            let content = match revision.deleted {
                true => "deleted".into(),
                false => String::from_utf8_lossy(&revision.content),
            };
            format!("{} fact {key} {content}", revision.lsn)
        }
    }
}

/// Every record, following `next`.
fn records(engine: &Engine, tenant: &str, mut query: RecordQuery) -> Vec<String> {
    let mut found = Vec::new();
    loop {
        let page = engine.records(tenant, query.clone()).unwrap();
        found.extend(page.records.iter().map(line));
        match page.next {
            Some(next) => query.before = Some(next),
            None => return found,
        }
    }
}

/// Every page of sessions, following `next`, as `total <sessions> <turns>:` and then
/// `agent/session=turns` for each session.
fn sessions(engine: &Engine, tenant: &str, limit: usize) -> Vec<String> {
    let (mut pages, mut after) = (Vec::new(), None);
    loop {
        let page = engine.sessions(tenant, SessionQuery { after, limit }).unwrap();
        let mut words = vec![format!("total {} {}:", page.total_sessions, page.total_turns)];
        words
            .extend(page.sessions.iter().map(|s| format!("{}/{}={}", s.agent, s.session, s.turns)));
        pages.push(words.join(" "));
        match page.next {
            Some(next) => after = Some(next),
            None => return pages,
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

#[test]
fn search_matches_plurals_of_words() {
    let engine = open(Arc::new(SimDisk::new(62)), options());
    for text in ["one Key", "two retries", "a policy"] {
        say(&engine, &scope("s"), text);
    }
    for (words, expected) in [
        ("keys", &["s:one Key"][..]),
        ("retries", &["s:two retries"]),
        ("policies", &["s:a policy"]),
    ] {
        assert_eq!(search(&engine, "tenant", q(words)), expected, "{words}");
    }
}

#[test]
fn reads_keep_order_across_segments() {
    let engine = open(Arc::new(SimDisk::new(63)), options());
    let pad = "x".repeat(1024);
    let texts: Vec<String> = (0..300).map(|i| format!("zebra {i} {pad}")).collect();
    texts.iter().for_each(|text| say(&engine, &scope("s"), text));
    let newest_first: Vec<String> = texts.iter().rev().map(|text| format!("s:{text}")).collect();
    let all = SearchQuery { limit: 1000, max_bytes: 8 << 20, ..q("zebra") };
    assert_eq!(search(&engine, "tenant", all), newest_first, "300 KiB spans 5 segments of 64 KiB");
    let window = engine.window(&scope("s"), zerodb_core::WindowQuery::Last(300)).unwrap();
    let read: Vec<String> =
        window.into_iter().map(|t| String::from_utf8(t.content).unwrap()).collect();
    assert_eq!(read, texts);
}

#[test]
fn sessions_and_records_list_what_a_tenant_wrote() {
    let disk = Arc::new(SimDisk::new(64));
    let engine = open(disk.clone(), options());
    let bot = |session: &str| Scope { agent: "bot".into(), ..scope(session) };
    let other = Scope { tenant: "other".into(), ..scope("s1") };
    // LSNs 1 and 3 create the tenants. Sessions are written out of byte order.
    say(&engine, &bot("s1"), "b1");
    say(&engine, &other, "secret");
    set(&engine, "k1", "v1");
    say(&engine, &scope("s2"), "c1");
    say(&engine, &scope("s2"), "c2");
    set(&engine, "k2", "gone");
    let fact =
        NewFact { key: "k1".into(), content: "other v1".into(), meta: vec![], agent: "a".into() };
    engine.fact_put("other", fact, None, None).unwrap();
    say(&engine, &scope("s1"), "a1");
    set(&engine, "k1", "v2");
    engine.fact_delete("tenant", "k2", "a", None, None).unwrap();
    say(&engine, &scope("s1"), "a2");
    say(&engine, &scope("s1"), "a3");

    let listed = ["total 3 6: agent/s1=3 agent/s2=2 bot/s1=1"];
    for (case, tenant, limit, pages) in [
        (
            "byte order of agent, then session, and totals on every page",
            "tenant",
            2,
            &["total 3 6: agent/s1=3 agent/s2=2", "total 3 6: bot/s1=1"][..],
        ),
        ("a full last page has no next", "tenant", 3, &listed[..]),
        (
            "1 per page with the cursor",
            "tenant",
            1,
            &["total 3 6: agent/s1=3", "total 3 6: agent/s2=2", "total 3 6: bot/s1=1"][..],
        ),
        ("1 page", "tenant", 1000, &listed[..]),
        ("tenant B sees none of A's", "other", 100, &["total 1 1: agent/s1=1"][..]),
        ("an unknown tenant", "nobody", 100, &["total 0 0:"][..]),
    ] {
        assert_eq!(sessions(&engine, tenant, limit), pages, "{case}");
    }

    let all = [
        "14 turn agent/s1#3 a3",
        "13 turn agent/s1#2 a2",
        "12 fact k2 deleted",
        "11 fact k1 v2",
        "10 turn agent/s1#1 a1",
        "8 fact k2 gone",
        "7 turn agent/s2#2 c2",
        "6 turn agent/s2#1 c1",
        "5 fact k1 v1",
        "2 turn bot/s1#1 b1",
    ];
    let of =
        |kind: &str| all.iter().filter(|line| line.contains(kind)).copied().collect::<Vec<_>>();
    let (turns, facts) = (of(" turn "), of(" fact "));
    let only = |kind| RecordQuery { kind: Some(kind), ..rq() };
    for (case, query, expected) in [
        ("newest first", rq(), &all[..]),
        ("turns only", only(RecordKind::Turn), &turns[..]),
        ("facts only, the delete and the superseded value too", only(RecordKind::Fact), &facts[..]),
        ("1 record per page", RecordQuery { limit: 1, ..rq() }, &all[..]),
        ("the first record always fits", RecordQuery { max_bytes: 1, ..rq() }, &all[..]),
        ("2 records per call", RecordQuery { visits: 2, ..rq() }, &all[..]),
        ("before pages below an LSN", RecordQuery { before: Some(11), ..rq() }, &all[4..]),
    ] {
        assert_eq!(records(&engine, "tenant", query), expected, "{case}");
    }
    let theirs = ["9 fact k1 other v1", "4 turn agent/s1#1 secret"];
    assert_eq!(records(&engine, "other", rq()), theirs, "tenant B sees none of A's");
    assert!(records(&engine, "nobody", rq()).is_empty(), "an unknown tenant");

    // Turns 14 and 13 cost 9 bytes each: agent, session and content.
    for (case, query, expected, next) in [
        ("a full page", RecordQuery { limit: 3, ..rq() }, &all[..3], Some(12)),
        (
            "a cut resumes at the record that did not fit",
            RecordQuery { max_bytes: 20, ..rq() },
            &all[..2],
            Some(13),
        ),
        (
            "the first record comes back over max_bytes",
            RecordQuery { max_bytes: 1, ..rq() },
            &all[..1],
            Some(14),
        ),
        (
            "skipped records count as visits",
            RecordQuery { visits: 3, ..only(RecordKind::Turn) },
            &all[..2],
            Some(12),
        ),
        (
            "the visit cap can end an empty page",
            RecordQuery { visits: 2, ..only(RecordKind::Fact) },
            &[][..],
            Some(13),
        ),
        (
            "other tenants' records count as visits",
            RecordQuery { before: Some(5), visits: 2, ..rq() },
            &[][..],
            Some(3),
        ),
        ("the walk reached LSN 1", RecordQuery { before: Some(3), ..rq() }, &all[9..], None),
    ] {
        let page = engine.records("tenant", query).unwrap();
        assert_eq!(page.records.iter().map(line).collect::<Vec<_>>(), expected, "{case}");
        assert_eq!(page.next, next, "{case}");
    }

    // Each record is as written: the turns `window` reads, the revisions `fact_history` reads.
    let page = engine.records("tenant", rq()).unwrap().records;
    let mut s1: Vec<_> = page
        .iter()
        .filter_map(|record| match record {
            Record::Turn { agent, session, turn } if agent == "agent" && session == "s1" => {
                Some(turn.clone())
            }
            _ => None,
        })
        .collect();
    s1.reverse();
    assert_eq!(s1, engine.window(&scope("s1"), WindowQuery::Last(10)).unwrap());
    for key in ["k1", "k2"] {
        let revisions: Vec<_> = page
            .iter()
            .filter_map(|record| match record {
                Record::Fact { key: k, revision } if k == key => Some(revision.clone()),
                _ => None,
            })
            .collect();
        let history = engine.fact_history("tenant", key, usize::MAX).unwrap();
        assert_eq!(revisions, history.revisions, "{key}");
    }
    for error in [
        engine.records("tenant", RecordQuery { limit: 0, ..rq() }).unwrap_err(),
        engine.records("tenant", RecordQuery { visits: 0, ..rq() }).unwrap_err(),
        engine.sessions("tenant", SessionQuery { after: None, limit: 0 }).unwrap_err(),
    ] {
        assert!(matches!(error.kind, ErrorKind::InvalidInput), "{error}");
    }

    // A reopen from a checkpoint restores cold session pages. Turn counts need none of them.
    engine.checkpoint().unwrap();
    drop(engine);
    let engine = open(disk.clone(), options());
    assert_eq!(sessions(&engine, "tenant", 1000), listed, "after reopen");
    assert_eq!(records(&engine, "tenant", rq()), all, "after reopen");

    // A failed sync leaves a reserved turn that never publishes. Its session is not listed.
    disk.fail_next_sync().unwrap();
    let error = engine.append(&bot("s2"), vec![turn(1)], None).unwrap_err();
    assert_eq!(error.outcome, Outcome::Unknown);
    assert_eq!(sessions(&engine, "tenant", 1000), listed, "a reserved-only session");
    assert_eq!(records(&engine, "tenant", rq()), all, "only durable records");
}
