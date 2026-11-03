use crate::support::{open, options, scope, turn};
use atticdb_core::{
    forget::Selector, Engine, EngineOptions, ErrorKind, Hit, NewFact, NewTurn, Outcome, Record,
    RecordKind, RecordQuery, Role, Scope, SearchQuery, SessionQuery, WindowQuery,
};
use atticdb_tape::SimDisk;
use std::sync::Arc;

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
/// `deleted`, a placeholder's `forgotten`.
fn line(record: &Record) -> String {
    match record {
        Record::Turn { agent, session, turn } => {
            let content = match turn.redacted {
                true => "forgotten".into(),
                false => String::from_utf8_lossy(&turn.content),
            };
            format!("{} turn {agent}/{session}#{} {content}", turn.lsn, turn.seq)
        }
        Record::Fact { key, revision } => {
            let content = match (revision.redacted, revision.deleted) {
                (true, _) => "forgotten".into(),
                (_, true) => "deleted".into(),
                _ => String::from_utf8_lossy(&revision.content),
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
    // Forgets hide hits. Another tenant can't hide this tenant's records (I11).
    let session = |name: &str| Selector::Session { agent: "agent".into(), session: name.into() };
    for (case, tenant, selector, expected) in [
        ("another tenant's LSNs", "other", Selector::Lsns(vec![2, 9, 10]), &all[..]),
        ("a session", "tenant", session("s2"), &[all[0], all[2]][..]),
        ("a fact key", "tenant", Selector::FactKey("repo/api/retry".into()), &all[2..]),
        ("an LSN", "tenant", Selector::Lsns(vec![2]), &[][..]),
    ] {
        engine.forget(tenant, selector, None, None).unwrap();
        assert_eq!(search(&engine, "tenant", q("retry")), expected, "{case}");
        assert_eq!(search(&engine, "other", q("retry")), ["s1:retry secret"], "{case}");
    }
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
    let window = engine.window(&scope("s"), atticdb_core::WindowQuery::Last(300)).unwrap();
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

    // Forgets. LSN 15: `other` names this tenant's LSNs, which stay visible (I11). 16: session
    // agent/s2. 17: key k1 up to its first value. 18: LSNs 1 and 3, which are no turn or fact
    // of this tenant, 13, and 14 and 1000 above `up_to_lsn`. 19 and 20: a session with no
    // turn and a key never set store nothing.
    let session = |name: &str| Selector::Session { agent: "agent".into(), session: name.into() };
    for (case, tenant, selector, up_to, counts) in [
        ("another tenant's LSNs", "other", Selector::Lsns(vec![2, 5, 6]), None, (0, 0, 3)),
        ("a session", "tenant", session("s2"), None, (1, 0, 3)),
        ("a key up to an LSN", "tenant", Selector::FactKey("k1".into()), Some(5), (1, 1, 3)),
        (
            "LSNs up to an LSN",
            "tenant",
            Selector::Lsns(vec![13, 1, 3, 14, 13, 1000]),
            Some(13),
            (1, 1, 6),
        ),
        ("a session with no turn", "tenant", session("none"), None, (1, 1, 6)),
        ("a key never set", "tenant", Selector::FactKey("never".into()), None, (1, 1, 6)),
    ] {
        engine.forget(tenant, selector, None, up_to).unwrap();
        let m = engine.metrics().unwrap();
        let got = (m.forgotten_sessions, m.forgotten_fact_keys, m.forgotten_lsns);
        assert_eq!(got, counts, "{case}");
        // Names, counts and totals are visible metadata. A forget never changes them.
        assert_eq!(sessions(&engine, "tenant", 1000), listed, "{case}");
    }
    let hidden = [
        "14 turn agent/s1#3 a3",
        "13 turn agent/s1#2 forgotten",
        "12 fact k2 deleted",
        "11 fact k1 v2",
        "10 turn agent/s1#1 a1",
        "8 fact k2 gone",
        "7 turn agent/s2#2 forgotten",
        "6 turn agent/s2#1 forgotten",
        "5 fact k1 forgotten",
        "2 turn bot/s1#1 b1",
    ];
    assert_eq!(records(&engine, "tenant", rq()), hidden);
    assert_eq!(records(&engine, "other", rq()), theirs);
    // A placeholder counts its identifiers only: 9, 7 and 3 bytes fill 19.
    let page = engine.records("tenant", RecordQuery { max_bytes: 19, ..rq() }).unwrap();
    assert_eq!(page.records.iter().map(line).collect::<Vec<_>>(), hidden[..3]);
    let page = engine.sessions("tenant", SessionQuery { after: None, limit: 10 }).unwrap();
    let cutoffs: Vec<_> = page.sessions.iter().map(|s| s.forgotten_up_to).collect();
    assert_eq!(cutoffs, [None, Some(16), None]);
    assert_eq!(page.as_of_lsn, 20, "the durable LSN after the last write");

    // A reopen from a checkpoint restores cold session pages. Turn counts need none of them.
    engine.checkpoint().unwrap();
    drop(engine);
    let engine = open(disk.clone(), options());
    assert_eq!(sessions(&engine, "tenant", 1000), listed, "after reopen");
    assert_eq!(records(&engine, "tenant", rq()), hidden, "after reopen");

    // A failed sync leaves a reserved turn that never publishes. Its session is not listed.
    disk.fail_next_sync().unwrap();
    let error = engine.append(&bot("s2"), vec![turn(1)], None).unwrap_err();
    assert_eq!(error.outcome, Outcome::Unknown);
    assert_eq!(sessions(&engine, "tenant", 1000), listed, "a reserved-only session");
    assert_eq!(records(&engine, "tenant", rq()), hidden, "only durable records");
}

/// Words chosen to hit the matcher's edges: case folding, final sigma, accents, punctuation
/// inside tokens, plurals with a singular, and words inside longer words.
const WORDS: [&str; 22] = [
    "Deploy",
    "deployment",
    "keys",
    "key",
    "policies",
    "policy",
    "café",
    "CAFÉ",
    "naïve",
    "ΣΟΦΙΑΣ",
    "σοφια",
    "foo.bar",
    "foo",
    "bar",
    "a",
    "x",
    "path/to/file.rs",
    "ERROR:",
    "error",
    "404",
    "über",
    "Übers",
];
const SEPARATORS: [&str; 6] = [" ", "\n", "\t", ", ", ".", "  "];
const QUERIES: [&str; 18] = [
    "deploy",
    "ploy",
    "keys",
    "policies",
    "oli",
    "café",
    "CAFÉ",
    "ΣΟΦΙΑΣ",
    "σοφιας",
    "o.b",
    "/to/",
    "error: 404",
    "a",
    "x bar",
    "übers",
    "nothing-matches",
    "é",
    "key deploy",
];

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
    fn text(&mut self) -> Vec<u8> {
        let mut text = Vec::new();
        for _ in 0..self.below(12) {
            text.extend_from_slice(WORDS[self.below(WORDS.len())].as_bytes());
            text.extend_from_slice(SEPARATORS[self.below(SEPARATORS.len())].as_bytes());
        }
        if self.below(10) == 0 {
            text.push(0xff);
        }
        if self.below(4) == 0 {
            // Long and repetitive, so it compresses.
            text = text.repeat(40);
        }
        text
    }
}

/// Waits until the word index covers every durable record or stopped growing.
fn indexed(engine: &Engine) {
    let started = std::time::Instant::now();
    loop {
        let m = engine.metrics().unwrap();
        if m.search_index_lsn == m.durable_lsn || m.search_index_stopped.is_some() {
            return;
        }
        assert!(started.elapsed().as_secs() < 20, "the index never caught up: {m:?}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Every page of every query, walked with `next`, hits and cursors included.
fn pages(engine: &Engine, tenant: &str) -> Vec<String> {
    let mut out = Vec::new();
    for query in QUERIES {
        for (limit, max_bytes) in [(1, 1 << 20), (3, 64), (100, 1 << 20)] {
            let mut before = None;
            loop {
                let query = SearchQuery { before, limit, max_bytes, ..q(query) };
                let page = engine.search(tenant, query.clone()).unwrap();
                out.push(format!("{query:?}: {:?} next={:?}", page.hits, page.next));
                match page.next {
                    Some(next) => before = Some(next),
                    None => break,
                }
            }
        }
    }
    out
}

/// Asserts no search hit is hidden: each turn reads back unredacted in its window, and each
/// fact is its key's current value.
fn no_hidden_hits(engine: &Engine, tenant: &str, case: &str) {
    for query in QUERIES {
        let mut before = None;
        loop {
            let page =
                engine.search(tenant, SearchQuery { before, limit: 100, ..q(query) }).unwrap();
            for hit in &page.hits {
                match hit {
                    Hit::Turn { agent, session, turn } => {
                        let scope = Scope {
                            tenant: tenant.into(),
                            agent: agent.clone(),
                            session: session.clone(),
                        };
                        let since = WindowQuery::Since { seq: turn.seq, limit: 1 };
                        let read = engine.window(&scope, since).unwrap();
                        assert!(!read[0].redacted && !turn.redacted, "{case}: {query} {turn:?}");
                    }
                    Hit::Fact(fact) => {
                        let current = engine.fact_get(tenant, &fact.key).unwrap();
                        assert_eq!(current.map(|f| f.lsn), Some(fact.lsn), "{case}: {query}");
                    }
                }
            }
            match page.next {
                Some(next) => before = Some(next),
                None => break,
            }
        }
    }
}

/// The word index only proposes candidates, so search must return exactly what a full walk
/// returns: same hits, same pages, same cursors. Before the index catches up, after it, when
/// its budget stops it, on compressed turns, and while it rebuilds after a reopen. Forgets
/// are in the corpus, and no page holds hidden content.
#[test]
fn search_with_the_index_equals_a_full_scan() {
    use atticdb_core::CompressionMode::{Off, On};
    for seed in 0..4 {
        let disks: [Arc<SimDisk>; 3] = std::array::from_fn(|_| Arc::new(SimDisk::new(seed)));
        let open_all = || {
            let opts = |bytes, mode, walk_first| {
                let mut opts = options();
                (opts.search_index_bytes, opts.compression.mode) = (bytes, mode);
                opts.search_walk_first = walk_first;
                opts
            };
            [
                open(disks[0].clone(), opts(0, Off, 0)),
                open(disks[1].clone(), opts(256 << 20, On, 0)),
                open(disks[2].clone(), opts(2048, Off, 7)),
            ]
        };
        let check = |engines: &[Engine; 3], case: &str| {
            for tenant in ["tenant", "other"] {
                let scan = pages(&engines[0], tenant);
                for (name, engine) in [("index", &engines[1]), ("tiny budget", &engines[2])] {
                    assert_eq!(
                        pages(engine, tenant),
                        scan,
                        "seed {seed}, {name}, {tenant}: {case}"
                    );
                }
                no_hidden_hits(&engines[1], tenant, &format!("seed {seed}, {tenant}: {case}"));
            }
        };
        let engines = open_all();
        let mut rng = Rng(seed);
        for step in 0..300 {
            let tenant = ["tenant", "other"][rng.below(2)];
            if rng.below(16) == 0 {
                let durable = engines[0].metrics().unwrap().durable_lsn.max(1) as usize;
                let selector = match rng.below(3) {
                    0 => Selector::Session {
                        agent: "agent".into(),
                        session: format!("s{}", rng.below(5)),
                    },
                    1 => Selector::FactKey(
                        ["repo/deploy-rule", "user/policies"][rng.below(2)].into(),
                    ),
                    _ => Selector::Lsns(
                        (0..1 + rng.below(3)).map(|_| 1 + rng.below(durable) as u64).collect(),
                    ),
                };
                let up_to = (rng.below(2) == 0).then(|| 1 + rng.below(durable) as u64);
                for engine in &engines {
                    engine.forget(tenant, selector.clone(), None, up_to).unwrap();
                }
            } else if rng.below(4) == 0 {
                let key = ["repo/deploy-rule", "user/policies", "café/notes"][rng.below(3)];
                let (delete, content) = (rng.below(5) == 0, rng.text());
                for engine in &engines {
                    if delete {
                        let _ = engine.fact_delete(tenant, key, "a", None, None);
                    } else {
                        let fact = NewFact {
                            key: key.into(),
                            content: content.clone(),
                            meta: vec![],
                            agent: "a".into(),
                        };
                        engine.fact_put(tenant, fact, None, None).unwrap();
                    }
                }
            } else {
                let scope = Scope { tenant: tenant.into(), ..scope(&format!("s{}", rng.below(5))) };
                let turns: Vec<NewTurn> = (0..1 + rng.below(3))
                    .map(|_| NewTurn { role: Role::User, content: rng.text(), meta: vec![] })
                    .collect();
                for engine in &engines {
                    engine.append(&scope, turns.clone(), None).unwrap();
                }
            }
            if step % 100 == 99 {
                check(&engines, &format!("step {step}, catching up"));
            }
        }
        engines.iter().for_each(indexed);
        check(&engines, "caught up");
        let m = engines[1].metrics().unwrap();
        assert!(m.search_index_terms > 0 && m.search_index_stopped.is_none(), "{m:?}");
        assert!(m.forgotten_sessions + m.forgotten_fact_keys + m.forgotten_lsns > 0, "{m:?}");
        assert!(m.compressed_turns > 0, "candidates come from compressed turns too");
        let stopped = engines[2].metrics().unwrap().search_index_stopped.unwrap_or_default();
        assert!(stopped.contains("budget"), "{stopped}");
        let off = engines[0].metrics().unwrap().search_index_stopped;
        assert_eq!(off.as_deref(), Some("off"));
        drop(engines);
        let engines = open_all();
        check(&engines, "rebuilding after a reopen");
        engines.iter().for_each(indexed);
        check(&engines, "rebuilt");
    }
}
