use crate::support::{self, append_body, serve, sim_engine, Dir, Via, TOKEN_A, TOKEN_B};
use atticdb::outcome::{self, Attempt, Merge};
use atticdb::serve::MAX_CONNECTIONS;
use atticdb_core::{EngineError, EngineOptions, ErrorKind, NewTurn, Role, Scope};
use atticdb_tape::{Error, Outcome, SimDisk};
use serde_json::{json, Value};
use std::sync::Arc;

fn seqs(window: &Value) -> Vec<u64> {
    window["turns"].as_array().unwrap().iter().map(|turn| turn["seq"].as_u64().unwrap()).collect()
}

/// What a retry must return unchanged: `seqs` and the LSNs for an append, `lsn` for a fact.
fn receipt(body: &Value) -> Value {
    json!([body["txn"], body["first_lsn"], body["last_lsn"], body["seqs"], body["lsn"]])
}

#[test]
fn writes_reads_and_retries_over_both_listeners() {
    let disk = SimDisk::new(1);
    let dir = Dir::new();
    let tune: fn(&mut EngineOptions) = |opts| opts.max_facts = 8;
    let engine = sim_engine(&disk, tune);
    let running = serve(engine.clone(), &dir, MAX_CONNECTIONS);
    let repeats = || {
        let (_, stats) = running.vias[1].call(None, None, "GET", "/v1/stats", None);
        stats["turn_repeats"].as_u64().unwrap()
    };
    let before = repeats();
    let (_, stats) = running.vias[1].call(None, None, "GET", "/v1/stats", None);
    let migration = [&stats["manifest_version"], &stats["compressing"], &stats["compressed_turns"]];
    assert_eq!(migration, [&json!(3), &json!(true), &json!(0)], "a new store compresses");
    let index = [&stats["search_index_lsn"], &stats["search_index_stopped"]];
    assert!(index[0].is_u64() && index[1].is_null(), "the word index runs: {stats}");
    let mut firsts = Vec::new();
    for via in &running.vias {
        let name = via.name();
        let session = format!("s-{name}");
        let first_body = append_body(&session, "k1", "hello");
        let (status, first) = via.a("POST", "/v1/append", Some(&first_body));
        assert_eq!(status, 200, "{name}: {first}");
        assert_eq!(first["outcome"], "committed", "{name}");
        assert_eq!(first["deduplicated"], false, "{name}");
        assert_eq!(first["seqs"], json!([1]), "{name}");

        // I4: the writer reads its own write right after the ack.
        let window_of = |query: &str| {
            via.a("GET", &format!("/v1/window?agent=a&session={session}&{query}"), None)
        };
        let (status, window) = window_of("last=10");
        assert_eq!(status, 200, "{name}");
        let turn = &window["turns"][0];
        assert_eq!(turn["seq"], 1, "{name}");
        assert_eq!(turn["lsn"], first["first_lsn"], "{name}");
        assert_eq!(turn["content"], "hello", "{name}");
        assert_eq!(turn["role"], "user", "{name}");
        assert_eq!(turn["meta"], json!({"k": 1}), "{name}");
        assert!(turn["commit_time_ns"].as_u64().unwrap() > 0, "{name}");
        assert!(turn.get("lossy").is_none(), "{name}");
        assert_eq!(window["truncated"], false, "{name}");
        assert_eq!(window["as_of_lsn"], first["last_lsn"], "{name}: read at the last ack");

        let (status, retry) = via.a("POST", "/v1/append", Some(&first_body));
        assert_eq!(status, 200, "{name}");
        assert_eq!(retry["deduplicated"], true, "{name}: retry");
        assert_eq!(receipt(&retry), receipt(&first), "{name}: retry keeps the receipt");

        let changed = append_body(&session, "k1", "changed");
        let (status, conflict) = via.a("POST", "/v1/append", Some(&changed));
        assert_eq!(status, 409, "{name}");
        let expected = json!({"outcome": "not_committed", "error": "idempotency_conflict"});
        assert_eq!(conflict, expected, "{name}");

        // Seqs 2 to 4 each hold 100 content bytes and 7 meta bytes.
        let mut more = append_body(&session, "k2", "");
        more["turns"] = ["b", "c", "d"]
            .map(|c| json!({"role": "assistant", "content": c.repeat(100), "meta": {"k": 1}}))
            .into();
        let (status, appended) = via.a("POST", "/v1/append", Some(&more));
        assert_eq!((status, &appended["seqs"]), (200, &json!([2, 3, 4])), "{name}");
        for (query, expected, truncated) in [
            ("last=10", vec![1, 2, 3, 4], false),
            ("last=5000", vec![1, 2, 3, 4], false),
            ("last=10&max_bytes=250", vec![3, 4], true),
            ("last=10&max_bytes=100", vec![], true),
            ("since_seq=2&limit=10&max_bytes=250", vec![2, 3], true),
            ("since_seq=1&limit=2", vec![1, 2], false),
        ] {
            let (status, window) = window_of(query);
            assert_eq!(status, 200, "{name} {query}");
            assert_eq!(seqs(&window), expected, "{name} {query}");
            assert_eq!(window["truncated"], truncated, "{name} {query}");
        }
        let (status, _) = window_of("last=1&since_seq=1");
        assert_eq!(status, 400, "{name}: last and since_seq together");
        // Engine limits answer 400 with a message that names the limit, not the input.
        let long = "x".repeat(256);
        for (row, field, value) in [
            ("no turns", "turns", json!([])),
            (
                "meta over 64 KiB",
                "turns",
                json!([{"role": "user", "content": "", "meta": {"m": "x".repeat(65536)}}]),
            ),
            ("agent over 255 bytes", "agent", json!(long)),
            ("session over 255 bytes", "session", json!(long)),
        ] {
            let mut body = append_body(&session, row, "v");
            body[field] = value;
            let (status, rejected) = via.a("POST", "/v1/append", Some(&body));
            assert_eq!(
                (status, &rejected["outcome"]),
                (400, &json!("not_committed")),
                "{name} {row}"
            );
            assert_eq!(rejected["error"], "invalid_input", "{name} {row}");
            let message = rejected["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("255 bytes") && !message.contains(&long),
                "{name} {row}: {rejected}"
            );
        }
        firsts.push(("/v1/append", name, first_body, first));

        // Facts over the same listener.
        let conflict = |error: &str| (409, json!({"outcome": "not_committed", "error": error}));
        // A `/`, a space, a `+` and a multibyte character, decoded from the query string.
        let key = format!("{name}/dir/a b+\u{e9}");
        let get =
            |key: &str| via.a("GET", &format!("/v1/facts/get?key={}", query_value(key)), None);
        let put = |body: &Value| via.a("POST", "/v1/facts/put", Some(body));
        let mut first_body = fact_body(&key, "v1", "p1");
        first_body["meta"] = json!({"m": 1});
        first_body["agent"] = json!("x");
        let (status, first) = put(&first_body);
        assert_eq!(status, 200, "{name}: {first}");
        assert_eq!(
            (&first["outcome"], &first["deduplicated"]),
            (&json!("committed"), &json!(false))
        );
        let (status, got) = get(&key);
        let fact = &got["fact"];
        assert_eq!(
            (status, &fact["key"], &fact["content"]),
            (200, &json!(key), &json!("v1")),
            "{name}"
        );
        assert_eq!(
            (&fact["agent"], &fact["meta"], &fact["lsn"]),
            (&json!("x"), &json!({"m": 1}), &first["lsn"])
        );
        assert!(fact["commit_time_ns"].as_u64().unwrap() > 0, "{name}");

        let lsn = first["lsn"].as_u64().unwrap();
        let mut second = fact_body(&key, "v2", "p2");
        second["if_lsn"] = json!(lsn - 1);
        assert_eq!(put(&second), conflict("conflict"), "{name}: stale if_lsn");
        assert_eq!(
            put(&fact_body(&key, "changed", "p1")),
            conflict("idempotency_conflict"),
            "{name}"
        );
        let (status, retry) = put(&first_body);
        assert_eq!((status, &retry["deduplicated"]), (200, &json!(true)), "{name}: retry");
        assert_eq!((&retry["txn"], &retry["lsn"]), (&first["txn"], &first["lsn"]), "{name}");
        second["if_lsn"] = json!(lsn);
        assert_eq!(put(&second).0, 200, "{name}: current if_lsn");
        let delete = json!({"key": key, "idem_key": "d1"});
        let (status, deleted) = via.a("POST", "/v1/facts/delete", Some(&delete));
        assert_eq!((status, &deleted["outcome"]), (200, &json!("committed")), "{name}");
        assert_eq!(get(&key), (200, json!({"fact": null})), "{name}: deleted");
        let history = |max_bytes: usize| {
            let target =
                format!("/v1/facts/history?key={}&max_bytes={max_bytes}", query_value(&key));
            via.a("GET", &target, None).1
        };
        let full = history(1000);
        let revisions = full["revisions"].as_array().unwrap();
        let shape: Vec<_> = revisions.iter().map(|r| (&r["deleted"], &r["content"])).collect();
        let (yes, no) = (json!(true), json!(false));
        let expected = [(&yes, &json!("")), (&no, &json!("v2")), (&no, &json!("v1"))];
        assert_eq!(shape, expected, "{name}: newest first, the tombstone first");
        assert_eq!((&full["truncated"], &revisions[0]["lsn"]), (&no, &deleted["lsn"]), "{name}");
        let cut = history(1);
        assert_eq!((cut["revisions"].as_array().unwrap().len(), &cut["truncated"]), (1, &yes));

        for n in 1..=3 {
            assert_eq!(put(&fact_body(&format!("{name}/list/{n}"), "v", &format!("l{n}"))).0, 200);
        }
        let list = |query: &str| {
            via.a("GET", &format!("/v1/facts/list?prefix={name}/list/&{query}"), None).1
        };
        let page = list("limit=2");
        assert_eq!(keys(&page), [format!("{name}/list/1"), format!("{name}/list/2")], "{name}");
        let page = list(&format!("after={}", query_value(page["next"].as_str().unwrap())));
        assert_eq!(
            (keys(&page), &page["next"]),
            (vec![format!("{name}/list/3").as_str()], &Value::Null)
        );
        for (case, body) in [
            ("1025-byte key", fact_body(&"k".repeat(1025), "v", "long")),
            ("content as a number", json!({"key": "n", "content": 1, "idem_key": "n"})),
            (
                "source refs",
                json!({"key": "n", "content": "v", "idem_key": "r", "source_refs": [1]}),
            ),
        ] {
            let (status, rejected) = put(&body);
            assert_eq!(
                (status, &rejected["error"]),
                (400, &json!("invalid_input")),
                "{name} {case}"
            );
        }
        firsts.push(("/v1/facts/put", name, first_body, first));
    }
    // The second listener's "hello" and three 100-byte turns repeat the first's. Other
    // tests share the counter, so this is a lower bound.
    assert!(repeats() - before >= 4, "repeats counted: {before} then {}", repeats());

    // Sessions and records answer alike on both listeners. Tenant B wrote nothing.
    let (lo, total) = (Some("127.0.0.1"), json!({"sessions": 2, "turns": 8}));
    let acked = running.vias[1].call(None, None, "GET", "/v1/stats", None).1["durable_lsn"].clone();
    for via in &running.vias {
        let name = via.name();
        let get = |target: &str| via.a("GET", target, None);
        let first = json!({"agent": "a", "session": "s-tcp"});
        let (status, page) = get("/v1/sessions?limit=1");
        assert_eq!(status, 200, "{name}: {page}");
        let session = |name: &str| {
            let mut listed = json!({"agent": "a", "session": name, "turns": 4});
            listed["forgotten_up_to"] = Value::Null;
            json!([listed])
        };
        let page_of = |listed: Value, next: Value| {
            let mut page = json!({"sessions": listed, "next": next, "total": total});
            page["as_of_lsn"] = acked.clone();
            page
        };
        assert_eq!(page, page_of(session("s-tcp"), first), "{name}");
        let page = get("/v1/sessions?after_agent=a&after_session=s-tcp").1;
        assert_eq!(page, page_of(session("s-unix"), Value::Null), "{name}");

        // The 3 newest records are the unix list facts. The delete comes right under them.
        let (status, page) = get("/v1/records?limit=3");
        assert_eq!(status, 200, "{name}: {page}");
        let newest = page["records"].as_array().unwrap();
        let shown: Vec<_> =
            newest.iter().map(|r| (&r["kind"], r["key"].as_str().unwrap())).collect();
        let fact = json!("fact");
        let expected = [(&fact, "unix/list/3"), (&fact, "unix/list/2"), (&fact, "unix/list/1")];
        assert_eq!(shown, expected, "{name}: newest first");
        assert_eq!(page["next"], newest[2]["lsn"], "{name}: a full page ends at its last record");
        // A fact record is its `history` revision plus kind and key. A turn record is its
        // `window` turn plus kind, agent and session.
        let key = "unix/dir/a b+\u{e9}";
        let below = format!("/v1/records?kind=fact&limit=1&before={}", page["next"]);
        let (_, deleted) = get(&below);
        let target = format!("/v1/facts/history?key={}", query_value(key));
        let mut revision = get(&target).1["revisions"][0].clone();
        (revision["kind"], revision["key"]) = (json!("fact"), json!(key));
        assert_eq!(deleted["records"], json!([revision]), "{name}: the delete");
        assert_eq!(deleted["next"], revision["lsn"], "{name}");
        let shown = (&revision["deleted"], &revision["content"]);
        assert_eq!(shown, (&json!(true), &json!("")), "{name}");
        let (_, turns) = get("/v1/records?kind=turn&limit=1");
        let mut turn = get("/v1/window?agent=a&session=s-unix&last=1").1["turns"][0].clone();
        (turn["kind"], turn["agent"], turn["session"]) =
            (json!("turn"), json!("a"), json!("s-unix"));
        assert_eq!(turns["records"], json!([turn]), "{name}: the newest turn");

        let a = Some(TOKEN_A);
        for (case, token, target, status) in [
            ("sessions without a token", None, "/v1/sessions", 401),
            ("records without a token", None, "/v1/records", 401),
            ("after_agent alone", a, "/v1/sessions?after_agent=a", 400),
            ("unknown parameter on sessions", a, "/v1/sessions?agent=a", 400),
            ("unknown parameter on records", a, "/v1/records?session=s", 400),
            ("kind is turn or fact", a, "/v1/records?kind=forget", 400),
            ("export without a token", None, "/v1/export?format=markdown", 401),
            ("export format is markdown", a, "/v1/export?format=html", 400),
            ("export max_bytes from 64 KiB", a, "/v1/export?format=markdown&max_bytes=65535", 400),
            (
                "export after_key with after_agent",
                a,
                "/v1/export?format=markdown&at=1&after_key=k&after_agent=a&after_session=s\
                 &after_seq=1&skip=0&offset=0",
                400,
            ),
        ] {
            let error = if status == 401 { "unauthorized" } else { "invalid_input" };
            let (got, body) = via.call(lo, token, "GET", target, None);
            assert_eq!((got, &body["error"]), (status, &json!(error)), "{name} {case}: {body}");
        }
        let zero = json!({"sessions": 0, "turns": 0});
        let as_of = &acked;
        for (target, nothing) in [
            (
                "/v1/sessions",
                json!({"sessions": [], "next": null, "total": zero, "as_of_lsn": as_of}),
            ),
            ("/v1/records", json!({"records": [], "next": null})),
        ] {
            let got = via.call(lo, Some(TOKEN_B), "GET", target, None);
            assert_eq!(got, (200, nothing), "{name}: tenant B sees none of A's: {target}");
        }
    }

    // Only direct Rust API writes can store content that is not UTF-8.
    let raw = Scope { tenant: "a".into(), agent: "a".into(), session: "raw".into() };
    let turn = NewTurn { role: Role::Tool, content: vec![b'o', 0xff, b'k'], meta: vec![] };
    engine.append(&raw, vec![turn], None).unwrap();
    let (_, window) = running.vias[1].a("GET", "/v1/window?agent=a&session=raw&last=1", None);
    let turn = &window["turns"][0];
    assert_eq!(turn["content"], "o\u{fffd}k");
    assert_eq!(
        (&turn["lossy"], &turn["meta"], &turn["role"]),
        (&json!(true), &Value::Null, &json!("tool"))
    );

    let full =
        json!({"outcome": "not_committed", "error": "resource_exhausted", "resource": "facts"});
    let new = fact_body("new", "v", "new");
    assert_eq!(running.vias[0].a("POST", "/v1/facts/put", Some(&new)), (429, full), "8 keys held");

    // Forgets: each selector over both listeners. Names, LSNs and counts stay (visible metadata).
    let unix = &running.vias[1];
    let stats = || unix.call(None, None, "GET", "/v1/stats", None).1;
    let counts =
        |stats: Value| stats_counts(&stats, ["facts", "fact_revisions", "compressed_turns"]);
    let listed = |page: Value| {
        let sessions = page["sessions"].as_array().unwrap().iter();
        let names = sessions.map(|s| [&s["agent"], &s["session"], &s["turns"]].map(Value::clone));
        (names.collect::<Vec<_>>(), page["total"].clone())
    };
    let sessions_before = listed(unix.a("GET", "/v1/sessions", None).1);
    let counts_before = counts(stats());
    let mut last = 0;
    for via in &running.vias {
        let name = via.name();
        let get = |target: &str| via.a("GET", target, None).1;
        let forget = |body: &Value| via.a("POST", "/v1/forget", Some(body));
        let window_target = format!("/v1/window?agent=a&session=s-{name}&last=10");
        let turns = get(&window_target)["turns"].clone();
        let lsn_of = |seq: usize| turns[seq - 1]["lsn"].as_u64().unwrap();
        let committed = |(status, body): (u16, Value), case: &str| {
            assert_eq!((status, &body["outcome"]), (200, &json!("committed")), "{name} {case}");
            body
        };

        // A session up to its seq 2. The cutoff is up_to_lsn, below the forget's own LSN.
        let up_to = lsn_of(2);
        let session = json!({"agent": "a", "session": format!("s-{name}")});
        let body =
            json!({"idem_key": format!("f1-{name}"), "session": session, "up_to_lsn": up_to});
        let forgot = committed(forget(&body), "session");
        assert_eq!((&forgot["deduplicated"], &forgot["cutoff"]), (&json!(false), &json!(up_to)));
        assert!(forgot["lsn"].as_u64().unwrap() > up_to && forgot["txn"].is_u64(), "{forgot}");
        let retry = committed(forget(&body), "retry");
        assert_eq!(retry["deduplicated"], true, "{name}: a retry writes nothing");
        assert_eq!(receipt(&retry), receipt(&forgot), "{name}: a retry keeps the receipt");
        assert_eq!(retry["cutoff"], forgot["cutoff"], "{name}");
        firsts.push(("/v1/forget", name, body, forgot));

        // A fact key, up to the forget itself: no current value, no list entry, no content.
        let key = format!("{name}/list/1");
        let body = json!({"idem_key": format!("f2-{name}"), "key": key});
        let forgot = committed(forget(&body), "key");
        assert_eq!(forgot["cutoff"], forgot["lsn"], "{name}: no up_to_lsn hides up to the forget");
        let target = format!("/v1/facts/get?key={key}");
        assert_eq!(get(&target), json!({"fact": null}), "{name}: a forgotten key has no value");
        let list = get(&format!("/v1/facts/list?prefix={name}/list/"));
        assert_eq!(keys(&list), [format!("{name}/list/2"), format!("{name}/list/3")]);
        let history = get(&format!("/v1/facts/history?key={key}"));
        let revision = &history["revisions"][0];
        assert_eq!(history["revisions"], json!([{"lsn": revision["lsn"], "redacted": true}]));

        // Listed LSNs: turn seq 3, and the oldest revision of the deleted key.
        let dir_key = format!("{name}/dir/a b+\u{e9}");
        let dir_history = format!("/v1/facts/history?key={}", query_value(&dir_key));
        let oldest = get(&dir_history)["revisions"][2]["lsn"].as_u64().unwrap();
        let body = json!({"idem_key": format!("f3-{name}"), "lsns": [lsn_of(3), oldest]});
        let forgot = committed(forget(&body), "lsns");
        last = forgot["lsn"].as_u64().unwrap();

        // Placeholders keep seq, LSN and, in records, kind and names. Seq 4 still shows.
        let window = get(&window_target);
        let hidden = (1..=3).map(|seq| json!({"seq": seq, "lsn": lsn_of(seq), "redacted": true}));
        let shown: Vec<_> = hidden.chain([turns[3].clone()]).collect();
        assert_eq!(window["turns"], json!(shown), "{name}: window");
        assert_eq!(window["as_of_lsn"], last, "{name}: read at the last ack");
        let history = get(&dir_history);
        let revisions = history["revisions"].as_array().unwrap();
        assert_eq!(revisions[1]["content"], "v2", "{name}: newer revisions still show");
        assert_eq!(revisions[2], json!({"lsn": oldest, "redacted": true}), "{name}: history");
        let records = |kind: &str, before: u64| {
            get(&format!("/v1/records?kind={kind}&limit=1&before={before}"))["records"][0].clone()
        };
        let turn = json!({"kind": "turn", "agent": "a", "session": format!("s-{name}"), "seq": 3,
                          "lsn": lsn_of(3), "redacted": true});
        assert_eq!(records("turn", lsn_of(3) + 1), turn, "{name}: turn record");
        let fact = json!({"kind": "fact", "key": dir_key, "lsn": oldest, "redacted": true});
        assert_eq!(records("fact", oldest + 1), fact, "{name}: fact record");
        let page = get("/v1/sessions");
        let cutoffs: Vec<_> = page["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["session"].as_str().unwrap(), s["forgotten_up_to"].as_u64()))
            .collect();
        assert!(cutoffs.contains(&(&format!("s-{name}"), Some(up_to))), "{name}: {page}");
    }
    // Names, turns, totals and the record counts are visible metadata: no forget changes them.
    let page = unix.a("GET", "/v1/sessions", None).1;
    assert_eq!(listed(page.clone()), sessions_before, "sessions and totals unchanged");
    assert_eq!(page["as_of_lsn"], last, "sessions read at the last ack");
    let after = stats();
    assert_eq!(counts(after.clone()), counts_before, "record counts unchanged");
    let gauges = ["forgotten_sessions", "forgotten_fact_keys", "forgotten_lsns"];
    assert_eq!(stats_counts(&after, gauges), [2, 2, 4], "{after}");
    let wire = unix.raw(b"GET /metrics HTTP/1.1\r\nConnection: close\r\n\r\n");
    for (gauge, value) in gauges.iter().zip(["2", "2", "4"]) {
        let line = format!("atticdb_{gauge} {value}");
        assert!(wire.lines().any(|got| got == line), "{line}: {wire}");
        assert!(wire.contains(&format!("# TYPE atticdb_{gauge} gauge\n")), "{gauge}: {wire}");
    }
    // Later writes show: the cutoff never covers them.
    for via in &running.vias {
        let name = via.name();
        let body = append_body(&format!("s-{name}"), "later", "later");
        assert_eq!(via.a("POST", "/v1/append", Some(&body)).0, 200, "{name}");
        let window = via.a("GET", &format!("/v1/window?agent=a&session=s-{name}&last=1"), None).1;
        assert_eq!(window["turns"][0]["content"], "later", "{name}: a later turn shows");
        let key = format!("{name}/list/1");
        assert_eq!(via.a("POST", "/v1/facts/put", Some(&fact_body(&key, "again", "again"))).0, 200);
        let fact = via.a("GET", &format!("/v1/facts/get?key={key}"), None).1;
        assert_eq!(fact["fact"]["content"], "again", "{name}: a later revision shows");
    }

    running.server.shutdown();
    assert_eq!(Arc::strong_count(&engine), 1, "shutdown releases the engine");
    drop(engine);
    // Reopen on the same directory. The old socket file is stale and gets replaced.
    let running = serve(sim_engine(&disk, tune), &dir, MAX_CONNECTIONS);
    for (target, name, body, first) in firsts {
        let via = &running.vias[usize::from(name == "unix")];
        let (status, retry) = via.a("POST", target, Some(&body));
        assert_eq!((status, &retry["deduplicated"]), (200, &json!(true)), "{name} {target}");
        assert_eq!(receipt(&retry), receipt(&first), "{name} {target}: original receipt");
    }
    running.server.shutdown();
}

/// A query value: `+` for a space, `/` as is, `%XX` for every other reserved byte.
fn query_value(text: &str) -> String {
    let byte = |byte: u8| match byte {
        b' ' => "+".to_owned(),
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
            char::from(byte).to_string()
        }
        _ => format!("%{byte:02X}"),
    };
    text.bytes().map(byte).collect()
}

fn fact_body(key: &str, content: &str, idem_key: &str) -> Value {
    json!({"key": key, "content": content, "idem_key": idem_key})
}

/// The named `/v1/stats` numbers.
fn stats_counts<const N: usize>(stats: &Value, names: [&str; N]) -> [u64; N] {
    names.map(|name| stats[name].as_u64().unwrap_or_else(|| panic!("{name}: {stats}")))
}

fn keys(page: &Value) -> Vec<&str> {
    page["facts"].as_array().unwrap().iter().map(|fact| fact["key"].as_str().unwrap()).collect()
}

/// `next` pages a key's history past the 16 revisions the index keeps. `before` takes it.
#[test]
fn fact_history_pages_with_before() {
    let dir = Dir::new();
    let running = serve(sim_engine(&SimDisk::new(5), |_| {}), &dir, MAX_CONNECTIONS);
    let unix = &running.vias[1];
    let put = |key: &str, n: usize| {
        let (status, put) = unix.a(
            "POST",
            "/v1/facts/put",
            Some(&fact_body(key, &n.to_string(), &format!("{key}{n}"))),
        );
        assert_eq!(status, 200, "{put}");
        put["lsn"].as_u64().unwrap()
    };
    let mut written: Vec<u64> = (1..=20).map(|n| put("k", n)).collect();
    written.reverse();
    let (mut seen, mut target, mut pages) = (Vec::new(), "/v1/facts/history?key=k".to_owned(), 0);
    loop {
        let (status, page) = unix.a("GET", &target, None);
        assert_eq!(status, 200, "{page}");
        let revisions = page["revisions"].as_array().unwrap();
        seen.extend(revisions.iter().map(|r| r["lsn"].as_u64().unwrap()));
        assert_eq!(page["truncated"], page["next"].is_u64(), "{page}");
        pages += 1;
        match page["next"].as_u64() {
            Some(next) => target = format!("/v1/facts/history?key=k&before={next}"),
            None => break,
        }
        assert!(pages < 20, "paging ends");
    }
    assert_eq!(seen, written, "every revision, newest first, once");
    assert!(pages > 1, "the first page stops at the index cap");

    for n in 1..=3 {
        put("three", n);
    }
    let (status, page) = unix.a("GET", "/v1/facts/history?key=three", None);
    assert_eq!((status, &page["next"], &page["truncated"]), (200, &Value::Null, &json!(false)));
    for target in [
        "/v1/facts/history?key=k&before=x",
        "/v1/facts/get?key=k&before=1",
        "/v1/facts/list?before=1",
    ] {
        let (status, rejected) = unix.a("GET", target, None);
        assert_eq!((status, &rejected["error"]), (400, &json!("invalid_input")), "{target}");
    }
    running.server.shutdown();
}

/// `order=relevance` ranks hits by their rarer words, with no `next`. No `order` keeps newest
/// first, where every word must appear. Any other `order` is 400.
#[test]
fn search_orders_by_relevance_or_newest() {
    let dir = Dir::new();
    let running = serve(sim_engine(&SimDisk::new(6), |_| {}), &dir, MAX_CONNECTIONS);
    let unix = &running.vias[1];
    let mut lsns = Vec::new();
    for (key, content) in [
        ("k1", "melanie painted a sunrise"),
        ("k2", "melanie went shopping"),
        ("k3", "melanie likes tea"),
    ] {
        let (status, put) = unix.a("POST", "/v1/append", Some(&append_body("s", key, content)));
        assert_eq!(status, 200, "{put}");
        lsns.push(put["first_lsn"].as_u64().unwrap());
    }
    let q = query_value("When did Melanie paint a sunrise?");
    let search = |query: &str| {
        let target = format!("/v1/search?q={q}{query}");
        let (status, page) = unix.a("GET", &target, None);
        assert_eq!(status, 200, "{target}: {page}");
        let hits = page["hits"].as_array().unwrap();
        let found: Vec<String> =
            hits.iter().map(|hit| hit["content"].as_str().unwrap().to_owned()).collect();
        (found, page["next"].clone())
    };
    let (sunrise, shopping, tea) =
        ("melanie painted a sunrise", "melanie went shopping", "melanie likes tea");
    for (query, expected) in [
        ("", vec![]),
        ("&order=newest", vec![]),
        ("&order=relevance", vec![sunrise, tea, shopping]),
        ("&order=relevance&limit=1", vec![sunrise]),
        (&format!("&order=relevance&before={}", lsns[2]), vec![sunrise, shopping]),
    ] {
        assert_eq!(search(query), (expected.iter().map(|s| s.to_string()).collect(), Value::Null));
    }
    let melanie = |query: &str| {
        let (status, page) = unix.a("GET", &format!("/v1/search?q=melanie{query}"), None);
        assert_eq!(status, 200, "{page}");
        page
    };
    let page = melanie("&limit=2");
    let found: Vec<&Value> =
        page["hits"].as_array().unwrap().iter().map(|h| &h["content"]).collect();
    assert_eq!(found, [&json!(tea), &json!(shopping)], "newest first: {page}");
    let next = page["next"].as_u64().unwrap_or_else(|| panic!("newest pages: {page}"));
    let rest = melanie(&format!("&before={next}"));
    assert_eq!((&rest["hits"][0]["content"], &rest["next"]), (&json!(sunrise), &Value::Null));
    for target in [
        format!("/v1/search?q={q}&order=x"),
        format!("/v1/search?q={q}&order="),
        "/v1/search?q=%3F%21&order=relevance".to_owned(),
    ] {
        let (status, rejected) = unix.a("GET", &target, None);
        assert_eq!((status, &rejected["error"]), (400, &json!("invalid_input")), "{target}");
    }
    // An id stays whole too, so the turn naming FIN-88 beats a newer one with only "fin".
    for (key, content) in [("k4", "closed FIN-88 today"), ("k5", "fin 88 notes")] {
        let (status, put) = unix.a("POST", "/v1/append", Some(&append_body("s", key, content)));
        assert_eq!(status, 200, "{put}");
    }
    let target = format!("/v1/search?q={}&order=relevance", query_value("Is FIN-88 fixed?"));
    let (status, page) = unix.a("GET", &target, None);
    let found: Vec<&Value> =
        page["hits"].as_array().unwrap().iter().map(|h| &h["content"]).collect();
    assert_eq!((status, found), (200, vec![&json!("closed FIN-88 today"), &json!("fin 88 notes")]));
    running.server.shutdown();
}

#[test]
fn auth_and_host_rules() {
    let dir = Dir::new();
    let engine = sim_engine(&SimDisk::new(2), |_| {});
    let running = serve(engine.clone(), &dir, MAX_CONNECTIONS);
    let [tcp, unix] = &running.vias;
    let (status, _) = unix.a("POST", "/v1/append", Some(&append_body("secret", "k", "a-data")));
    assert_eq!(status, 200);

    let window = "/v1/window?agent=a&session=secret&last=10";
    let bad = "token-x-0123456789abcdefghijklmnopqrstuv";
    let (lo, evil, wrong) = (Some("127.0.0.1"), Some("evil.example"), Some(bad));
    let (a, b, health, stats) = (Some(TOKEN_A), Some(TOKEN_B), "/healthz", "/v1/stats");
    // (case, via, Host, token, target, status, error or the number of A's turns read)
    let rows = [
        ("no token", tcp, lo, None, window, 401, Err("unauthorized")),
        ("bad token", tcp, lo, wrong, window, 401, Err("unauthorized")),
        ("bad token on unix", unix, None, wrong, window, 401, Err("unauthorized")),
        ("tenant B reads A's session", tcp, lo, b, window, 200, Ok(0)),
        ("tenant A", tcp, lo, a, window, 200, Ok(1)),
        ("localhost with port", tcp, Some("LocalHost:7425"), a, window, 200, Ok(1)),
        ("ipv6 loopback", tcp, Some("[::1]:1"), a, window, 200, Ok(1)),
        ("foreign host on tcp", tcp, evil, a, window, 403, Err("forbidden_host")),
        ("loopback prefix", tcp, Some("127.0.0.1.evil"), a, window, 403, Err("forbidden_host")),
        ("missing host on tcp", tcp, None, a, window, 403, Err("forbidden_host")),
        ("foreign host on unix", unix, evil, a, window, 200, Ok(1)),
        ("missing host on unix", unix, None, a, window, 200, Ok(1)),
        ("health without token", tcp, lo, None, health, 200, Ok(0)),
        ("stats on unix without token", unix, None, None, stats, 200, Ok(0)),
        ("stats on tcp", tcp, lo, a, stats, 404, Err("not_found")),
        ("stats on tcp without token", tcp, lo, None, stats, 401, Err("unauthorized")),
        ("health with foreign host", tcp, evil, None, health, 403, Err("forbidden_host")),
        ("forget is POST only", tcp, lo, a, "/v1/forget", 405, Err("method_not_allowed")),
    ];
    for (case, via, host, token, target, status, expect) in rows {
        let (got, body) = via.call(host, token, "GET", target, None);
        assert_eq!(got, status, "{case}: {body}");
        match expect {
            Err(error) => {
                assert_eq!(body, json!({"outcome": "not_committed", "error": error}), "{case}")
            }
            Ok(count) if target == window => {
                let turns = body["turns"].as_array().unwrap();
                assert_eq!(turns.len(), count, "{case}");
                assert!(turns.iter().all(|turn| turn["content"] == "a-data"), "{case}");
            }
            Ok(_) => assert_eq!(body["instance"], running.instance.as_str(), "{case}"),
        }
    }

    // I11: tenant B writing the same names gets its own session, never A's.
    let b_data = append_body("secret", "k", "b-data");
    let (status, appended) = tcp.call(lo, b, "POST", "/v1/append", Some(&b_data));
    assert_eq!((status, &appended["seqs"]), (200, &json!([1])), "tenant B write");
    let (_, a) = tcp.a("GET", window, None);
    assert_eq!(a["turns"].as_array().unwrap().len(), 1, "A still sees only its turn");
    let rule = json!({"key": "secret/rule", "content": "a-rule", "idem_key": "f"});
    assert_eq!(tcp.a("POST", "/v1/facts/put", Some(&rule)).0, 200);
    for (target, nothing) in [
        ("/v1/facts/get?key=secret/rule", json!({"fact": null})),
        ("/v1/facts/list?prefix=", json!({"facts": [], "next": null})),
    ] {
        let got = tcp.call(lo, b, "GET", target, None);
        assert_eq!(got, (200, nothing), "tenant B sees nothing of A's facts: {target}");
    }
    assert_eq!(tcp.a("GET", "/v1/facts/get?key=secret/rule", None).1["fact"]["content"], "a-rule");

    // I11: tenant B's forgets name A's session, key and LSN, and hide only B's own records.
    let a_turn = tcp.a("GET", window, None).1["turns"][0].clone();
    let session = json!({"agent": "a", "session": "secret"});
    for (case, selector) in [
        ("session", json!({"session": session})),
        ("key", json!({"key": "secret/rule"})),
        ("lsns", json!({"lsns": [a_turn["lsn"]]})),
    ] {
        let mut body = selector;
        body["idem_key"] = json!(case);
        let (status, forgot) = tcp.call(lo, b, "POST", "/v1/forget", Some(&body));
        assert_eq!((status, &forgot["outcome"]), (200, &json!("committed")), "B forgets {case}");
    }
    assert_eq!(tcp.a("GET", window, None).1["turns"], json!([a_turn]), "A's turn still shows");
    assert_eq!(tcp.a("GET", "/v1/facts/get?key=secret/rule", None).1["fact"]["content"], "a-rule");
    let (_, b_window) = tcp.call(lo, b, "GET", window, None);
    assert_eq!(b_window["turns"][0]["redacted"], true, "B's own turn is hidden: {b_window}");
    // Exactly 1 selector. LSNs and up_to_lsn start at 1. The retry key is required.
    let one = "pass exactly 1 of session {agent, session}, key or lsns";
    for (case, body, message) in [
        ("2 selectors", json!({"idem_key": "x", "key": "k", "lsns": [1]}), one),
        ("no selector", json!({"idem_key": "x"}), one),
        ("empty list", json!({"idem_key": "x", "lsns": []}), one),
        ("empty key", json!({"idem_key": "x", "key": ""}), one),
        (
            "session without agent",
            json!({"idem_key": "x", "session": {"session": "s"}}),
            "agent must be a string",
        ),
        ("lsn 0", json!({"idem_key": "x", "lsns": [3, 0]}), "lsns must be whole numbers from 1"),
        (
            "up_to_lsn 0",
            json!({"idem_key": "x", "key": "k", "up_to_lsn": 0}),
            "up_to_lsn must be a whole number from 1",
        ),
        ("no idem_key", json!({"key": "k"}), "idem_key must be a string"),
        (
            "1025 lsns",
            json!({"idem_key": "x", "lsns": (1..=1025).collect::<Vec<u64>>()}),
            "lsns holds 1 to 1024",
        ),
    ] {
        let (status, rejected) = tcp.a("POST", "/v1/forget", Some(&body));
        let expected = json!({"outcome": "not_committed", "error": "invalid_input"});
        assert_eq!(
            (status, &rejected["outcome"], &rejected["error"]),
            (400, &expected["outcome"], &expected["error"]),
            "{case}"
        );
        let said = rejected["message"].as_str().unwrap_or_default();
        assert!(said.contains(message), "{case}: {rejected}");
    }

    // Auth runs before the body is read: a bad token declaring 8 MiB gets 401 and a
    // closed connection without sending a byte of body.
    let head = format!(
        "POST /v1/append HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {bad}\r\n\
         Content-Length: {}\r\n\r\n",
        8 << 20
    );
    let started = std::time::Instant::now();
    let wire = tcp.raw(head.as_bytes());
    // Exactly these bytes: the studio's browser headers never reach the store's answers.
    let body = json!({"outcome": "not_committed", "error": "unauthorized"}).to_string();
    let fields = format!("Content-Type: application/json\r\nContent-Length: {}", body.len());
    assert_eq!(wire, format!("HTTP/1.1 401 \r\n{fields}\r\nConnection: close\r\n\r\n{body}"));
    assert!(started.elapsed().as_secs() < 5, "closed without waiting for the body");
    running.server.shutdown();

    // Past the connection cap a new connection gets 503 busy and is closed.
    let running = serve(engine, &dir, 2);
    let unix = &running.vias[1];
    let Via::Unix(path) = unix else { unreachable!() };
    let idle = [0, 1].map(|_| std::os::unix::net::UnixStream::connect(path).unwrap());
    // It answers on accept without reading, so send nothing: closing over unread bytes
    // makes Linux reset the connection before the client reads the answer.
    let wire = unix.raw(b"");
    let (head, body) = wire.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 503 "), "{wire}");
    let busy = json!({"outcome": "not_committed", "error": "busy"});
    assert_eq!(serde_json::from_str::<Value>(body).unwrap(), busy);
    // Stats count it. Ask over an accepted connection, since the cap is still full.
    use std::io::{Read, Write};
    let mut conn = &idle[0];
    conn.write_all(b"GET /v1/stats HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
    let mut wire = String::new();
    conn.read_to_string(&mut wire).unwrap();
    let stats: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(stats["busy_replies"].as_u64().unwrap() >= 1, "{wire}");
    drop(idle);
    // Shutdown needs no connection to the socket path, which may be gone or replaced.
    std::fs::remove_file(path).unwrap();
    running.server.shutdown();
}

/// `/healthz` reports the version `atticdb --version` prints, on both listeners, with no token.
/// So a client can tell a server left on old code after the binary is replaced.
#[test]
fn healthz_reports_version() {
    let out = support::command(env!("CARGO_BIN_EXE_atticdb")).arg("--version").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed = String::from_utf8(out.stdout).unwrap();
    let version = printed.trim_end().strip_prefix("atticdb ").unwrap().to_owned();
    assert_eq!(version, atticdb::VERSION);
    let (disk, dir) = (SimDisk::new(4), Dir::new());
    let running = serve(sim_engine(&disk, |_| {}), &dir, MAX_CONNECTIONS);
    for via in &running.vias {
        let (status, body) = via.call(Some("127.0.0.1"), None, "GET", "/healthz", None);
        assert_eq!(status, 200, "{}: {body}", via.name());
        assert_eq!(body["version"], json!(version), "{}: {body}", via.name());
    }
    running.server.shutdown();
}

#[test]
fn health_tracks_engine_state() {
    let disk = SimDisk::new(3);
    let dir = Dir::new();
    let engine = sim_engine(&disk, |o| {
        (o.tape.free_space_floor, o.max_sessions) = (Some(4096), 1);
        // The 2.2 MB repeated-letter turns below must not compress under the 4 MiB batch.
        o.compression.mode = atticdb_core::CompressionMode::Off;
    });
    let running = serve(engine, &dir, MAX_CONNECTIONS);
    let unix = &running.vias[1];
    let (instance, version) = (running.instance.as_str(), atticdb::VERSION);
    let health = || unix.call(None, None, "GET", "/healthz", None);
    let stats = || {
        let (status, stats) = unix.call(None, None, "GET", "/v1/stats", None);
        assert_eq!(status, 200, "stats serve in every state: {stats}");
        (stats["health"].clone(), stats["durable_lsn"].clone(), stats["facts"].clone())
    };
    let append = |key: &str, content: &str| {
        unix.a("POST", "/v1/append", Some(&append_body("s", key, content)))
    };
    let contents = || {
        let (status, window) = unix.a("GET", "/v1/window?agent=a&session=s&last=100", None);
        assert_eq!(status, 200, "windows keep serving");
        window["turns"].as_array().unwrap().iter().map(|t| t["content"].clone()).collect::<Vec<_>>()
    };
    assert_eq!(health(), (200, json!({"status": "ok", "instance": instance, "version": version})));

    // Over the 4 MiB tape batch but under the 8 MiB body cap.
    let mut big = append_body("s", "big", "");
    big["turns"] =
        ["x", "y"].map(|c| json!({"role": "user", "content": c.repeat(2_200_000)})).into();
    let too_large = json!({"outcome": "not_committed", "error": "too_large"});
    assert_eq!(unix.a("POST", "/v1/append", Some(&big)), (413, too_large.clone()));
    // More than 1,024 turns is rejected before the engine encodes them.
    let mut many = append_body("s", "many", "");
    many["turns"] = vec![json!({"role": "user", "content": ""}); 1025].into();
    assert_eq!(unix.a("POST", "/v1/append", Some(&many)), (413, too_large.clone()));
    let head = format!(
        "POST /v1/append HTTP/1.1\r\nAuthorization: Bearer {TOKEN_A}\r\nContent-Length: {}\r\n\r\n",
        (8 << 20) + 1
    );
    let wire = unix.raw(head.as_bytes());
    let length = too_large.to_string().len();
    let fields = format!("Content-Type: application/json\r\nContent-Length: {length}");
    assert_eq!(wire, format!("HTTP/1.1 413 \r\n{fields}\r\nConnection: close\r\n\r\n{too_large}"));
    assert_eq!(health().1["status"], "ok", "a rejected batch changes nothing");

    // I14: below the floor, writes stop without loss and reads go on.
    assert_eq!(append("first", "one").0, 200);
    let fact =
        |content: &str, key: &str| json!({"key": "rule", "content": content, "idem_key": key});
    assert_eq!(unix.a("POST", "/v1/facts/put", Some(&fact("kept", "f1"))).0, 200);
    let second = append_body("s2", "other", "v");
    let exhausted =
        json!({"outcome": "not_committed", "error": "resource_exhausted", "resource": "sessions"});
    assert_eq!(
        unix.a("POST", "/v1/append", Some(&second)),
        (429, exhausted),
        "one session allowed"
    );
    let capacity = SimDisk::DEFAULT_CAPACITY;
    let free = atticdb_tape::Io::available_bytes(&disk, "db".as_ref()).unwrap();
    disk.set_capacity(capacity - free + 4095).unwrap();
    let disk_full = json!({"outcome": "not_committed", "error": "disk_full"});
    assert_eq!(append("floor", "two"), (507, disk_full.clone()));
    assert_eq!(unix.a("POST", "/v1/facts/put", Some(&fact("new", "f2"))), (507, disk_full));
    let (status, rule) = unix.a("GET", "/v1/facts/get?key=rule", None);
    assert_eq!((status, &rule["fact"]["content"]), (200, &json!("kept")), "fact reads serve");
    assert_eq!(
        health(),
        (200, json!({"status": "read_only", "instance": instance, "version": version}))
    );
    let (_, kept) = unix.a("GET", "/v1/facts/get?key=rule", None);
    assert_eq!(stats(), (json!("read_only"), kept["fact"]["lsn"].clone(), json!(1)));
    // The same numbers for Prometheus, on the socket only.
    let wire = unix.raw(b"GET /metrics HTTP/1.1\r\nConnection: close\r\n\r\n");
    let (head, text) = wire.split_once("\r\n\r\n").unwrap();
    assert!(head.contains("Content-Type: text/plain; version=0.0.4"), "{head}");
    let value = |name: &str| {
        let line = text.lines().find(|line| line.split(' ').next() == Some(name));
        line.and_then(|line| line.split(' ').nth(1)).unwrap_or_else(|| panic!("{name}: {text}"))
    };
    assert_eq!((value("atticdb_read_only"), value("atticdb_facts")), ("1", "1"), "{text}");
    let types = ["writer_groups_total counter", "facts gauge", "writer_sync_seconds_total counter"];
    for line in types.map(|t| format!("# TYPE atticdb_{t}")) {
        assert!(text.lines().any(|got| got == line), "{line}: {text}");
    }
    let written: f64 = value("atticdb_writer_write_seconds_total").parse().unwrap();
    assert!(written > 0.0, "the committed appends' writer time is counted: {text}");
    value("atticdb_writer_sync_max_seconds");
    value("atticdb_checkpoint_snapshot_lock_seconds");
    assert!(value("atticdb_turn_content_bytes_bucket{le=\"64\"}").parse::<u64>().unwrap() >= 1);
    let count = value("atticdb_append_ack_seconds_count");
    assert_eq!(value("atticdb_append_ack_seconds_bucket{le=\"+Inf\"}"), count, "{text}");
    assert!(count.parse::<u64>().unwrap() >= 1, "the committed append is counted: {text}");
    let tcp = &running.vias[0];
    assert_eq!(tcp.call(Some("127.0.0.1"), None, "GET", "/metrics", None).0, 401, "not on TCP");
    assert_eq!(contents(), ["one"]);
    disk.set_capacity(capacity).unwrap();
    let (status, committed) = append("floor", "two");
    assert_eq!((status, &committed["deduplicated"]), (200, &json!(false)), "{committed}");
    assert_eq!(append("floor", "two").1["deduplicated"], true);
    assert_eq!(contents(), ["one", "two"], "the retried key committed once");
    assert_eq!(health().1["status"], "ok");

    // An ambiguous sync poisons the writer.
    disk.fail_next_sync().unwrap();
    let unknown = append("sync", "three");
    assert_eq!(unknown, (500, json!({"outcome": "unknown", "error": "storage"})));
    assert_eq!(
        health(),
        (503, json!({"status": "poisoned", "instance": instance, "version": version}))
    );
    assert_eq!(stats().0, "poisoned");
    assert_eq!(contents(), ["one", "two"], "reads keep the published view");
    // A failed sync publishes no dedup entry, so the retry gets not_committed from the
    // poisoned writer. Only the outcome rule keeps the caller at unknown.
    let retry = append("sync", "three");
    assert_eq!(retry, (503, json!({"outcome": "not_committed", "error": "poisoned"})));
    let mut merge = Merge::default();
    for body in [&unknown.1, &retry.1] {
        merge.record(Attempt::Answered(outcome::parse(body["outcome"].as_str().unwrap()).unwrap()));
    }
    assert_eq!(merge.finish(), Outcome::Unknown);
    running.server.shutdown();

    // The engine has no public hook to fail a receipt after commit, or a space-error
    // cleanup, so these two rows drive the response mapping directly.
    let failure = |kind, outcome| Err(EngineError { kind, outcome });
    for (case, result, status, body) in [
        (
            "receipt failure after commit",
            failure(ErrorKind::Poisoned, Outcome::Committed),
            200,
            json!({"outcome": "committed", "receipt": null, "error": "poisoned"}),
        ),
        (
            "space error with a failed cleanup",
            failure(ErrorKind::Storage(Error::DiskFull), Outcome::Unknown),
            507,
            json!({"outcome": "unknown", "error": "disk_full"}),
        ),
    ] {
        let response = atticdb::api::append_response(result);
        assert_eq!(response.status, status, "{case}");
        assert_eq!(serde_json::from_slice::<Value>(&response.body).unwrap(), body, "{case}");
    }
}

/// A child forked by another test thread holds a copy of every descriptor until it execs. So
/// an old listener can outlive `shutdown` and answer the reopen's probe (#219). Here a child
/// holds a copy as its stdin for 1 s, and `serve` must wait it out. The test checks the copy
/// still answers right before `serve`, and that the child is gone once `serve` returns. So it
/// can't pass without the wait.
#[test]
fn serve_waits_out_a_copy_of_the_old_listener() {
    let dir = Dir::new();
    let path = dir.0.join("a");
    let engine = sim_engine(&SimDisk::new(6), |_| {});
    let (listener, _) = atticdb::listen::install_unix(&path, None, "0123456789abcdef").unwrap();
    let copy = std::os::fd::OwnedFd::from(listener.try_clone().unwrap());
    let mut child =
        support::command("sleep").arg("1").stdin(std::process::Stdio::from(copy)).spawn().unwrap();
    drop(listener);
    assert!(std::os::unix::net::UnixStream::connect(&path).is_ok(), "the copy should answer");
    let running = serve(engine, &dir, MAX_CONNECTIONS);
    assert!(child.try_wait().unwrap().is_some(), "serve returned while the copy listened");
    assert_eq!(running.vias[1].call(None, None, "GET", "/healthz", None).0, 200);
    running.server.shutdown();
    assert!(child.wait().unwrap().success());
}

/// Once a lost `LOCK` stops the server, an open keep-alive connection is closed at once. A
/// write sent on it after the stop is not committed (#227).
#[test]
fn lost_lock_closes_keep_alive_connections() {
    use atticdb::http::Stream;
    use atticdb::serve::Exit;
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    use std::io::Read;
    use std::time::{Duration, Instant};
    let dir = Dir::new();
    let lock = dir.0.join("LOCK");
    std::fs::write(&lock, "").unwrap();
    let engine = sim_engine(&SimDisk::new(7), |_| {});
    let running =
        support::serve_watching(engine.clone(), &dir, MAX_CONNECTIONS, Some(lock.clone()));
    let durable = || atticdb::api::stats_fields(&engine).unwrap()["durable_lsn"].clone();
    let before = durable();
    let mut streams: Vec<(&str, Box<dyn Stream>)> = running
        .vias
        .iter()
        .map(|via| match via {
            Via::Tcp(addr) => ("tcp", Box::new(std::net::TcpStream::connect(addr).unwrap()) as _),
            Via::Unix(path) => {
                ("unix", Box::new(std::os::unix::net::UnixStream::connect(path).unwrap()) as _)
            }
        })
        .collect();
    let auth = format!("Bearer {TOKEN_A}");
    let headers = [("Host", "127.0.0.1"), ("Authorization", &auth), ("Connection", "keep-alive")];
    let send = |stream: &mut dyn Stream, method: &str, target: &str, body: &[u8]| {
        let deadline = Instant::now() + Duration::from_secs(10);
        atticdb::http::exchange(stream, method, target, &headers, body, deadline, 1 << 20)
    };
    for (name, stream) in &mut streams {
        let reply = send(&mut **stream, "GET", "/healthz", b"").unwrap();
        assert_eq!(reply.status, 200, "{name}: kept alive");
        // Set while open: macOS fails `setsockopt` once the peer has closed.
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    }
    std::fs::remove_file(&lock).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let server = running.server;
    let waiter = std::thread::spawn(move || {
        let _ = tx.send(server.wait());
        server
    });
    let why = rx.recv_timeout(Duration::from_secs(10));
    assert_eq!(why, Ok(Exit::LockLost), "stops within 10 s");
    for (name, stream) in &mut streams {
        // The server shut its read side, so its connection thread ends and closes.
        let read = stream.read(&mut [0; 64]);
        let timed_out = |e: &std::io::Error| matches!(e.kind(), WouldBlock | TimedOut);
        assert!(matches!(&read, Ok(0)) || read.as_ref().is_err_and(|e| !timed_out(e)), "{name}");
        let body = append_body("s", name, "after the stop").to_string();
        let reply = send(&mut **stream, "POST", "/v1/append", body.as_bytes());
        assert!(reply.as_ref().map_or(true, |reply| reply.status != 200), "{name}: {reply:?}");
    }
    assert_eq!(durable(), before, "nothing was committed");
    let target = "/v1/window?agent=a&session=s&last=10";
    let window = atticdb::api::window(&engine, "a", target);
    let window: Value = serde_json::from_slice(&window.body).unwrap();
    assert_eq!(window["turns"], json!([]), "{window}");
    waiter.join().unwrap().shutdown();
}
