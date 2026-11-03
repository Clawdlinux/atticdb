use crate::support::{private_file, serve, sim_engine, Dir, Process, Via, TOKEN_A};
use atticdb_core::{forget::Selector, Engine, NewFact, NewTurn, Role, Scope};
use atticdb_tape::SimDisk;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

const PAGE: usize = 65536;

fn turn(content: impl Into<Vec<u8>>) -> NewTurn {
    NewTurn { role: Role::User, content: content.into(), meta: br#"{"k":1}"#.to_vec() }
}

/// Appends turns to a session of tenant `a`. Returns their LSNs.
fn append(engine: &Engine, agent: &str, session: &str, turns: Vec<NewTurn>) -> Vec<u64> {
    let scope = Scope { tenant: "a".into(), agent: agent.into(), session: session.into() };
    let receipt = engine.append(&scope, turns, None).unwrap();
    (receipt.first_lsn..=receipt.last_lsn).collect()
}

fn fact(engine: &Engine, key: &str, content: &str) {
    let fact = NewFact {
        key: key.into(),
        content: content.into(),
        meta: br#"{"m":"x"}"#.to_vec(),
        agent: "ag".into(),
    };
    engine.fact_put("a", fact, None, None).unwrap();
}

fn forget(engine: &Engine, selector: Selector) {
    engine.forget("a", selector, None, None).unwrap();
}

/// Percent-encodes a query value.
fn query_value(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => char::from(byte).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// The page request a cursor names, with each field overridable as raw query text.
fn target(next: &Value, over: &[(&str, &str)]) -> String {
    let mut fields = vec![("at", next["at"].to_string())];
    match next.get("key") {
        Some(key) => fields.push(("after_key", query_value(key.as_str().unwrap()))),
        None => {
            fields.push(("after_agent", query_value(next["agent"].as_str().unwrap())));
            fields.push(("after_session", query_value(next["session"].as_str().unwrap())));
            fields.push(("after_seq", next["seq"].to_string()));
        }
    }
    fields.push(("skip", next["skip"].to_string()));
    fields.push(("offset", next["offset"].to_string()));
    for (name, value) in over {
        fields.iter_mut().find(|(field, _)| field == name).unwrap().1 = (*value).to_owned();
    }
    let query: Vec<String> = fields.iter().map(|(name, value)| format!("{name}={value}")).collect();
    format!("/v1/export?format=markdown&max_bytes={PAGE}&{}", query.join("&"))
}

/// An export read page by page, with each file joined from its chunks.
struct Walk {
    at: u64,
    files: BTreeMap<String, String>,
    /// The cursor after each page, with its body length.
    pages: Vec<(Value, usize)>,
    /// Pages that carried a chunk of each path.
    spans: BTreeMap<String, usize>,
}

/// Reads every page. `between` runs after each page, with its number from 1.
fn walk(via: &Via, mut between: impl FnMut(usize)) -> Walk {
    let mut out = Walk { at: 0, files: BTreeMap::new(), pages: Vec::new(), spans: BTreeMap::new() };
    let mut request = format!("/v1/export?format=markdown&max_bytes={PAGE}");
    let mut last = String::new();
    loop {
        let (status, page) = via.a("GET", &request, None);
        assert_eq!(status, 200, "{request}: {page}");
        // The server writes the same bytes `serde_json` writes for the parsed tree.
        let len = page.to_string().len();
        assert!(len <= PAGE, "page {}: {len} bytes", out.pages.len() + 1);
        let at = page["at"].as_u64().unwrap();
        assert!(out.at == 0 || out.at == at, "every page reads at 1 LSN");
        out.at = at;
        let chunks = page["chunks"].as_array().unwrap();
        assert!(!chunks.is_empty(), "every page carries markdown");
        for chunk in chunks {
            let path = chunk["path"].as_str().unwrap().to_owned();
            let offset = chunk["offset"].as_u64().unwrap() as usize;
            let markdown = chunk["markdown"].as_str().unwrap();
            if offset == 0 {
                assert!(out.files.insert(path.clone(), String::new()).is_none(), "{path} once");
            } else {
                assert_eq!(path, last, "offset {offset}: a file continues where it stopped");
            }
            let file = out.files.get_mut(&path).unwrap();
            assert_eq!(file.len(), offset, "{path}: offsets are continuous");
            file.push_str(markdown);
            *out.spans.entry(path.clone()).or_default() += 1;
            last = path;
        }
        out.pages.push((page["next"].clone(), len));
        between(out.pages.len());
        if page["next"].is_null() {
            return out;
        }
        request = target(&page["next"], &[]);
    }
}

/// Every file under `dir`, by its path relative to it, with its mode. Dirs end in `/`.
fn tree(dir: &Path) -> BTreeMap<String, (u32, Option<String>)> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(dir).unwrap().to_str().unwrap().to_owned();
            let mode = std::fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o777;
            if path.is_dir() {
                out.insert(format!("{name}/"), (mode, None));
                todo.push(path);
            } else {
                out.insert(name, (mode, Some(std::fs::read_to_string(&path).unwrap())));
            }
        }
    }
    out
}

/// Runs `atticdb export <dir> ...` against `socket` as tenant `a`.
fn export(dir: &Dir, socket: &Path, out: &str) -> (Option<i32>, String) {
    let token = dir.0.join("t");
    if !token.exists() {
        private_file(dir, "t", TOKEN_A);
    }
    let url = format!("unix://{}", socket.display());
    let args = ["export", out, "--url", &url, "--token-file", token.to_str().unwrap()];
    let mut process = Process::spawn(&dir.0, &[&args[..], &["--max-bytes", "65536"]].concat());
    let (status, output) = process.wait_up_to(std::time::Duration::from_secs(120));
    (status.code(), output)
}

#[test]
fn export_writes_one_file_per_session_and_facts_at_one_lsn() {
    let disk = SimDisk::new(11);
    let dir = Dir::new();
    let engine = sim_engine(&disk, |_| {});
    let running = serve(engine.clone(), &dir, 16);
    let via = &running.vias[1];

    let key = "q\"\n\u{1}\u{7}\tk";
    fact(&engine, key, "value with \"quotes\"");
    fact(&engine, "file/repo/notes.md", "{\"about\":\"notes\"}");
    fact(&engine, "gone-key", "SECRET-KEY");
    let big = "abcdefgh".repeat(1 << 17);
    let accents = "\u{e9}".repeat(100_000);
    let long = "n".repeat(255);
    let upper = "A".repeat(100);
    append(&engine, "", "", vec![turn("empty names")]);
    append(&engine, "App", "s", vec![turn("compressed ".repeat(40))]);
    let app = append(
        &engine,
        "app",
        "s",
        vec![turn("one"), turn("SECRET-TURN"), turn(&b"\xff\xfe raw"[..])],
    );
    append(&engine, "../x", "con", vec![turn("traversal")]);
    append(&engine, "a.", "big", vec![turn(big.clone()), turn("after big")]);
    append(&engine, "a.", "accents", vec![turn(accents.clone())]);
    append(&engine, &long, "long", vec![turn("long agent")]);
    append(&engine, &upper, "x", vec![turn("upper agent")]);
    append(&engine, "app", "gone", vec![turn("SECRET-SESSION"), turn("SECRET-SESSION 2")]);
    forget(&engine, Selector::Lsns(vec![app[1]]));
    forget(&engine, Selector::FactKey("gone-key".into()));
    let session = Selector::Session { agent: "app".into(), session: "gone".into() };
    forget(&engine, session);

    // A write after page 1 is past the export's LSN, so no page shows it.
    let manual = walk(via, |page| {
        if page == 1 {
            append(&engine, "app", "s", vec![turn("LATE-WRITE")]);
            append(&engine, "app", "late", vec![turn("LATE-WRITE")]);
            fact(&engine, "late-key", "LATE-WRITE");
        }
    });
    let at = manual.at;
    let mut paths: BTreeSet<&str> = manual.files.keys().map(String::as_str).collect();
    let hashed: Vec<&str> = paths.iter().copied().filter(|path| path.contains('~')).collect();
    assert_eq!(hashed.len(), 2, "names past 200 encoded bytes are cut and hashed: {paths:?}");
    for (path, prefix) in
        hashed.iter().zip([format!("{}~", "%41".repeat(60)), format!("{}~", "n".repeat(180))])
    {
        assert!(path.strip_prefix("sessions/").unwrap().starts_with(&prefix), "{path}");
        paths.remove(path);
    }
    let expected = [
        "facts.md",
        "sessions/%/%.md",
        "sessions/%41pp/s.md",
        "sessions/%2e%2e%2fx/%63on.md",
        "sessions/a%2e/accents.md",
        "sessions/a%2e/big.md",
        "sessions/app/gone.md",
        "sessions/app/s.md",
    ];
    assert_eq!(paths, BTreeSet::from(expected), "1 file per session, App and app apart");

    let all: String = manual.files.values().map(String::as_str).collect();
    for secret in ["SECRET-KEY", "SECRET-TURN", "SECRET-SESSION", "LATE-WRITE", "late-key"] {
        assert!(!all.contains(secret), "{secret} is not exported");
    }
    let lsns = all.match_indices("lsn ").map(|(i, _)| &all[i + 4..]);
    let lsns = lsns.map(|rest| rest.split(|c: char| !c.is_ascii_digit()).next().unwrap());
    let lsns: Vec<u64> = lsns.map(|digits| digits.parse().unwrap()).collect();
    assert!(lsns.len() >= 12 && lsns.iter().all(|lsn| *lsn <= at), "nothing past {at}");

    let facts = &manual.files["facts.md"];
    assert!(
        facts.starts_with(&format!("# Facts, LSN {at}\n\n## \"file/repo/notes.md\"\n")),
        "{facts}"
    );
    let quoted = serde_json::to_string(key).unwrap();
    assert!(facts.contains(&format!("\n## {quoted}\nlsn ")), "a key is 1 JSON line: {facts}");
    assert!(facts.contains("```\nvalue with \"quotes\"\n```\n{\"m\":\"x\"}\n\n"), "{facts}");
    assert!(!facts.contains("gone-key"), "a forgotten key is left out");
    let app = &manual.files["sessions/app/s.md"];
    assert!(app.starts_with("# \"app\" / \"s\"\n\n## #1 user, "), "{app}");
    assert!(app.contains("\n## #2 forgotten\n\n## #3 user, "), "a placeholder: {app}");
    assert!(app.contains(" (lossy)\n\n```\n\u{fffd}\u{fffd} raw\n```\n{\"k\":1}\n\n"), "{app}");
    let gone = &manual.files["sessions/app/gone.md"];
    assert_eq!(gone, "# \"app\" / \"gone\"\n\n## #1 forgotten\n\n## #2 forgotten\n\n");
    assert!(manual.files["sessions/%41pp/s.md"].contains(&"compressed ".repeat(40)));
    let big_file = &manual.files["sessions/a%2e/big.md"];
    assert!(big_file.contains(&format!("```\n{big}\n```\n")) && big_file.contains("after big"));
    assert!(manual.spans["sessions/a%2e/big.md"] >= 16, "the 1 MiB turn spans pages");
    assert!(manual.files["sessions/%/%.md"].starts_with("# \"\" / \"\"\n\n"));

    // Malformed cursors, from a real one inside the turn of 100,000 é.
    let inside = manual.pages.iter().map(|(next, _)| next).find(|next| {
        next["session"] == "accents" && next["skip"].as_u64().is_some_and(|skip| skip > 100)
    });
    let real = inside.unwrap().clone();
    let (skip, offset) = (real["skip"].as_u64().unwrap(), real["offset"].as_u64().unwrap());
    let (mid, past) = ((skip + 1).to_string(), (skip + 1_000_000).to_string());
    let (mid_offset, past_offset) = ((offset + 1).to_string(), (offset + 1_000_000).to_string());
    for (case, over, status, error) in [
        (
            "skip lands mid-char",
            vec![("skip", mid.as_str()), ("offset", &mid_offset)],
            400,
            "invalid_input",
        ),
        (
            "skip past the entry's end",
            vec![("skip", &past), ("offset", &past_offset)],
            409,
            "conflict",
        ),
        ("non-numeric skip", vec![("skip", "1x")], 400, "invalid_input"),
        ("skip past offset", vec![("offset", "0")], 400, "invalid_input"),
        ("a traversal name is only a name", vec![("after_agent", "..%2Fx")], 409, "conflict"),
        (
            "a traversal name at an entry's start",
            vec![("after_agent", "..%2Fx"), ("skip", "0"), ("offset", "0")],
            200,
            "",
        ),
    ] {
        let (got, body) = via.a("GET", &target(&real, &over), None);
        if status == 200 {
            // `../x` has no session `accents`, so the page starts at the next one, `../x`
            // `con`. Its path is encoded like any other.
            let chunk = &body["chunks"][0];
            let path = json!("sessions/%2e%2e%2fx/%63on.md");
            assert_eq!((got, &chunk["path"], &chunk["offset"]), (200, &path, &json!(0)), "{case}");
            continue;
        }
        assert_eq!((got, &body["error"]), (status, &json!(error)), "{case}: {body}");
        let (again, page) = via.a("GET", &target(&real, &[]), None);
        assert_eq!(again, 200, "{case}: the real cursor still works: {page}");
        assert_eq!(page["chunks"][0]["offset"], real["offset"], "{case}: the server echoes offset");
    }
    let (_, body) = via.a("GET", &target(&real, &[("skip", &mid), ("offset", &mid_offset)]), None);
    assert_eq!(body["message"], "skip is not on a character boundary");

    // The CLI writes the same files as a walk at its own LSN, private, and nothing outside.
    let out = dir.0.join("out");
    let (code, output) = export(&dir, &via_path(via), "out");
    assert_eq!(code, Some(0), "{output}");
    let hot = engine.metrics().unwrap().hot_cache_bytes;
    let fresh = walk(via, |_| {});
    assert_eq!(engine.metrics().unwrap().hot_cache_bytes, hot, "an export never fills the cache");
    let files = tree(&out);
    let mut joined = BTreeMap::new();
    for (name, (mode, text)) in &files {
        match text {
            Some(text) => {
                assert_eq!(*mode, 0o600, "{name}");
                joined.insert(name.clone(), text.clone());
            }
            None => assert_eq!(*mode, 0o700, "{name}"),
        }
    }
    assert_eq!(joined, fresh.files, "each file equals its parts joined");
    assert!(joined["sessions/app/s.md"].contains("LATE-WRITE"), "the CLI ran after the write");
    assert!(joined.contains_key("sessions/%2e%2e%2fx/%63on.md"), "../x stays inside <dir>");
    assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o700);
    let beside: Vec<String> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let stray = beside.iter().filter(|name| name.contains("partial") || name.ends_with(".md"));
    assert_eq!(stray.count(), 0, "no partial folder and no file outside <dir>: {beside:?}");

    let (code, output) = export(&dir, &via_path(via), "out");
    assert_eq!(code, Some(1), "an existing <dir> is refused: {output}");
    assert!(output.contains("exists"), "{output}");
    assert_eq!(tree(&out), files, "and left as it was");
}

fn via_path(via: &Via) -> std::path::PathBuf {
    match via {
        Via::Unix(path) => path.clone(),
        Via::Tcp(_) => unreachable!("exports run over the Unix socket"),
    }
}

/// A Unix socket proxy to `upstream`. Before the request numbered `when`, it runs `hook`.
/// Returns the status of each answer.
fn proxy(
    socket: &Path,
    upstream: &Path,
    when: usize,
    hook: impl Fn() + Send + 'static,
) -> Arc<Mutex<Vec<u16>>> {
    let listener = UnixListener::bind(socket).unwrap();
    let (upstream, statuses) = (upstream.to_path_buf(), Arc::new(Mutex::new(Vec::new())));
    let seen = statuses.clone();
    std::thread::spawn(move || {
        for (i, client) in listener.incoming().enumerate() {
            let mut client = client.unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") && client.read(&mut byte).unwrap() == 1 {
                request.push(byte[0]);
            }
            if i + 1 == when {
                hook();
            }
            let mut server = UnixStream::connect(&upstream).unwrap();
            server.write_all(&request).unwrap();
            let mut answer = Vec::new();
            server.read_to_end(&mut answer).unwrap();
            let status = String::from_utf8_lossy(&answer[9..12]).parse().unwrap();
            seen.lock().unwrap().push(status);
            let _ = client.write_all(&answer);
        }
    });
    statuses
}

#[test]
fn export_restarts_when_a_forget_cuts_an_entry() {
    let disk = SimDisk::new(12);
    let dir = Dir::new();
    let engine = sim_engine(&disk, |_| {});
    let running = serve(engine.clone(), &dir, 16);
    let via = &running.vias[1];
    let big = "abcdefgh".repeat(1 << 17);
    let lsns = append(&engine, "copilot", "atticdb", vec![turn(big.clone()), turn("small")]);

    let (status, first) =
        via.a("GET", &format!("/v1/export?format=markdown&max_bytes={PAGE}"), None);
    assert_eq!(status, 200, "{first}");
    let next = first["next"].clone();
    assert!(next["seq"] == 1 && next["skip"].as_u64().unwrap() > 0, "page 1 ends inside: {next}");

    // The CLI's 2nd request finds the turn forgotten: 409. It starts again and finishes.
    let hooked = engine.clone();
    let forget_big = move || forget(&hooked, Selector::Lsns(vec![lsns[0]]));
    let statuses = proxy(&dir.0.join("p"), &via_path(via), 2, forget_big);
    let (code, output) = export(&dir, &dir.0.join("p"), "out");
    assert_eq!(code, Some(0), "{output}");
    assert_eq!(statuses.lock().unwrap().clone(), vec![200, 409, 200], "1 restart");
    let file = std::fs::read_to_string(dir.0.join("out/sessions/copilot/atticdb.md")).unwrap();
    assert!(file.starts_with("# \"copilot\" / \"atticdb\"\n\n## #1 forgotten\n\n## #2 user, "));
    assert!(!file.contains("abcdefgh"), "the turn is a placeholder");

    let (status, body) = via.a("GET", &target(&next, &[]), None);
    let expected = json!({"error": "conflict", "message": "the export fell behind; start again"});
    assert_eq!((status, body), (409, expected), "a forget mid-entry stops the old cursor");
}
