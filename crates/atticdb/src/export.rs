//! Markdown export. `GET /v1/export?format=markdown` renders a tenant's memory at 1 durable
//! LSN, in pages of `{path, offset, markdown}` chunks. `atticdb export <dir>` writes them into
//! `facts.md` and `sessions/<agent>/<session>.md`.
//!
//! Pages cut inside files, so 1 huge session still fits `max_bytes`. A cursor names the entry
//! to resume at. `skip` counts the bytes of that entry already sent, and `offset` those of its
//! file. The server keeps no export state between pages.
//!
//! The client writes into `<dir>.partial-<hex>` and renames it to `<dir>` at the end. So
//! `<dir>` either doesn't exist or holds a complete export. Each chunk path must pass a strict
//! grammar before any write. So no path can leave the target directory.

use crate::api::{self, DEFAULT_WINDOW_BYTES, MAX_WINDOW_BYTES, MAX_WINDOW_TURNS};
use crate::client::{Store, StoreFlags};
use crate::http::Response;
#[cfg(not(unix))]
use crate::platform::{DirBuilderExt, OpenOptionsExt};
use atticdb_core::{Engine, EngineError, ErrorKind, ExportEntry, ExportPos, ExportQuery, Role};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{ErrorKind as IoKind, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Smallest `max_bytes` of an export page.
pub const MIN_EXPORT_BYTES: usize = 64 << 10;
/// Bytes kept out of the chunk budget for the envelope and the worst `next`. A 1,024-byte key
/// at 6 bytes per byte is 6 KiB.
const RESERVE: usize = 8 << 10;
/// The message of a 409 `conflict`.
const CONFLICT: &str = "the export fell behind; start again";
/// The longest an encoded name gets before it is cut and hashed.
const NAME_BYTES: usize = 200;
/// How many times the client restarts after a 409.
const RESTARTS: usize = 3;
/// How long the client waits for 1 page, connect included.
const PAGE_DEADLINE: Duration = Duration::from_secs(60);

/// Where an export page resumes. `skip` counts the entry's markdown bytes already sent, and
/// `offset` those of its file.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Cursor {
    at: u64,
    pos: ExportPos,
    skip: usize,
    offset: u64,
}

/// A piece of 1 file's markdown, starting `offset` bytes into it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Chunk {
    path: String,
    offset: u64,
    markdown: String,
}

/// `GET /v1/export?format=markdown[&at=&after_*=&skip=&offset=&max_bytes=]`. The body is at
/// most `max_bytes`, exactly.
pub(crate) fn page(engine: &Engine, tenant: &str, target: &str) -> Response {
    let (cursor, max_bytes) = match parse(target) {
        Ok(parsed) => parsed,
        Err(message) => return invalid(&message),
    };
    match build(engine, tenant, cursor.as_ref(), max_bytes) {
        Ok((at, chunks, next)) => {
            let body = body(at, &chunks, next.as_ref());
            let bytes = serde_json::to_vec(&body).unwrap_or_default();
            debug_assert!(bytes.len() <= max_bytes, "{} > {max_bytes}", bytes.len());
            debug_assert_eq!(bytes.len(), body_len(at, &chunks, next.as_ref()));
            let json = "application/json";
            Response {
                status: 200,
                body: bytes,
                content_type: json,
                headers: Vec::new(),
                _held: None,
            }
        }
        Err(response) => response,
    }
}

fn invalid(message: &str) -> Response {
    Response::json(400, &json!({"error": "invalid_input", "message": message}))
}

fn conflict() -> Response {
    Response::json(409, &json!({"error": "conflict", "message": CONFLICT}))
}

const CURSOR: &str = "pass at, then after_key, or after_agent, after_session and after_seq, then \
                      skip and offset";

/// The query: an optional cursor and `max_bytes`.
fn parse(target: &str) -> Result<(Option<Cursor>, usize), String> {
    let mut params = api::params(target)?;
    if params.remove("format").as_deref() != Some("markdown") {
        return Err("format must be markdown".into());
    }
    let number = |value: Option<String>, name: &str| {
        let parse =
            |text: String| text.parse::<u64>().map_err(|_| format!("{name} must be a number"));
        value.map(parse).transpose()
    };
    let max_bytes = match number(params.remove("max_bytes"), "max_bytes")? {
        None => DEFAULT_WINDOW_BYTES,
        Some(bytes) => usize::try_from(bytes)
            .ok()
            .filter(|bytes| (MIN_EXPORT_BYTES..=MAX_WINDOW_BYTES).contains(bytes))
            .ok_or(format!("max_bytes must be {MIN_EXPORT_BYTES} to {MAX_WINDOW_BYTES}"))?,
    };
    let names = ["at", "after_key", "after_agent", "after_session", "after_seq", "skip", "offset"];
    let [at, key, agent, session, seq, skip, offset] = names.map(|name| params.remove(name));
    if !params.is_empty() {
        return Err("unknown query parameter".into());
    }
    let pos = match (&at, key, agent, session, seq, &skip, &offset) {
        (None, None, None, None, None, None, None) => return Ok((None, max_bytes)),
        (Some(_), Some(key), None, None, None, Some(_), Some(_)) if key.len() <= 1024 => {
            ExportPos::Fact { key }
        }
        (Some(_), None, Some(agent), Some(session), Some(seq), Some(_), Some(_))
            if agent.len() <= 255 && session.len() <= 255 =>
        {
            let seq = number(Some(seq), "after_seq")?.unwrap_or_default();
            ExportPos::Turn { agent, session, seq }
        }
        _ => return Err(CURSOR.into()),
    };
    let at = number(at, "at")?.unwrap_or_default();
    let skip = number(skip, "skip")?.unwrap_or_default();
    let offset = number(offset, "offset")?.unwrap_or_default();
    // Far below u64::MAX, so adding a page's bytes to it can't overflow.
    if offset > i64::MAX as u64 {
        return Err(format!("offset must be at most {}", i64::MAX));
    }
    // A cursor's offset counts the skipped bytes of its entry too.
    let skip = usize::try_from(skip).ok().filter(|skip| *skip as u64 <= offset);
    let skip = skip.ok_or("skip must be at most offset")?;
    Ok((Some(Cursor { at, pos, skip, offset }), max_bytes))
}

/// Renders 1 page: chunks within `max_bytes` less the reserve, and the cursor after them.
fn build(
    engine: &Engine,
    tenant: &str,
    cursor: Option<&Cursor>,
    max_bytes: usize,
) -> Result<(u64, Vec<Chunk>, Option<Cursor>), Response> {
    let budget = max_bytes - RESERVE;
    let query = ExportQuery {
        at: cursor.map(|cursor| cursor.at),
        after: cursor.map(|cursor| cursor.pos.clone()),
        max_bytes: budget,
        max_items: MAX_WINDOW_TURNS,
    };
    let page = engine.export(tenant, query).map_err(failed)?;
    let at = page.at;
    // A cursor inside an entry needs that entry first, and visible. A forget may have hidden
    // it, and an empty page has no entry to check in the loop below.
    if let Some(cursor) = cursor.filter(|cursor| cursor.skip > 0) {
        let shown = page.entries.first().is_some_and(|entry| {
            let hidden = matches!(entry, ExportEntry::Turn { turn, .. } if turn.redacted);
            entry.pos() == cursor.pos && !hidden
        });
        if !shown {
            return Err(conflict());
        }
    }
    // The file the next entry continues, and its bytes before that entry.
    let (mut file, mut written) = match cursor {
        Some(cursor) => (path_of(&cursor.pos), cursor.offset - cursor.skip as u64),
        None => (String::new(), 0),
    };
    let (mut chunks, mut used, mut next) = (Vec::<Chunk>::new(), 0usize, None);
    for (i, entry) in page.entries.iter().enumerate() {
        let pos = entry.pos();
        let path = path_of(&pos);
        if path != file {
            (file, written) = (path.clone(), 0);
        }
        let markdown = render(entry, at, written == 0);
        let mut start = 0;
        // The cursor's entry, re-rendered. Checked before any slice, so no cursor can panic.
        if let Some(cursor) = cursor.filter(|cursor| i == 0 && cursor.skip > 0) {
            if cursor.skip > markdown.len() {
                return Err(conflict());
            }
            if !markdown.is_char_boundary(cursor.skip) {
                return Err(invalid("skip is not on a character boundary"));
            }
            start = cursor.skip;
        }
        let rest = &markdown[start..];
        let offset = written + start as u64;
        if !rest.is_empty() && chunks.last().is_none_or(|chunk| chunk.path != path) {
            let cost = chunk_len(&path, offset) + usize::from(!chunks.is_empty());
            let first = rest.chars().next().map_or(0, char_len);
            if used + cost + first > budget {
                next = Some(Cursor { at, pos, skip: start, offset });
                break;
            }
            used += cost;
            chunks.push(Chunk { path, offset, markdown: String::new() });
        }
        let (taken, cost) = fit(rest, budget - used);
        used += cost;
        if let Some(chunk) = chunks.last_mut() {
            chunk.markdown.push_str(&rest[..taken]);
        }
        if taken < rest.len() {
            let (skip, offset) = (start + taken, offset + taken as u64);
            next = Some(Cursor { at, pos, skip, offset });
            break;
        }
        written += markdown.len() as u64;
    }
    if next.is_none() {
        next = page.next.map(|pos| {
            let offset = if path_of(&pos) == file { written } else { 0 };
            Cursor { at, pos, skip: 0, offset }
        });
    }
    Ok((at, chunks, next))
}

fn failed(error: EngineError) -> Response {
    match error.kind {
        ErrorKind::Conflict => conflict(),
        ErrorKind::InvalidInput => invalid("at must be at most the durable LSN"),
        kind => {
            let (status, name) = api::classify(&kind);
            Response::json(status, &json!({"error": name}))
        }
    }
}

fn body(at: u64, chunks: &[Chunk], next: Option<&Cursor>) -> Value {
    let chunks: Vec<Value> = chunks
        .iter()
        .map(
            |chunk| json!({"path": chunk.path, "offset": chunk.offset, "markdown": chunk.markdown}),
        )
        .collect();
    json!({"format": "markdown", "at": at, "chunks": chunks, "next": next.map(cursor_json)})
}

fn cursor_json(cursor: &Cursor) -> Value {
    let Cursor { at, pos, skip, offset } = cursor;
    match pos {
        ExportPos::Fact { key } => {
            json!({"at": at, "key": key, "skip": skip, "offset": offset})
        }
        ExportPos::Turn { agent, session, seq } => json!({"at": at, "agent": agent,
            "session": session, "seq": seq, "skip": skip, "offset": offset}),
    }
}

/// The body's length as `serde_json` writes it, from the same parts `build` counts.
fn body_len(at: u64, chunks: &[Chunk], next: Option<&Cursor>) -> usize {
    let envelope = serde_json::to_vec(&body(at, &[], next)).map_or(0, |bytes| bytes.len());
    let commas = chunks.len().saturating_sub(1);
    let chunks =
        chunks.iter().map(|chunk| chunk_len(&chunk.path, chunk.offset) + json_len(&chunk.markdown));
    envelope + commas + chunks.sum::<usize>()
}

/// A chunk's bytes with empty markdown: `{"markdown":"","offset":n,"path":"..."}`.
fn chunk_len(path: &str, offset: u64) -> usize {
    let empty = json!({"path": path, "offset": offset, "markdown": ""});
    serde_json::to_vec(&empty).map_or(usize::MAX / 2, |bytes| bytes.len())
}

/// A char's bytes inside a `serde_json` string.
fn char_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

fn json_len(text: &str) -> usize {
    text.chars().map(char_len).sum()
}

/// The longest prefix of `text` that costs at most `room` bytes as JSON: its byte length
/// and its cost. It ends on a char boundary.
fn fit(text: &str, room: usize) -> (usize, usize) {
    let mut cost = 0;
    for (at, c) in text.char_indices() {
        let next = cost + char_len(c);
        if next > room {
            return (at, cost);
        }
        cost = next;
    }
    (text.len(), cost)
}

/// `facts.md`, or `sessions/<agent>/<session>.md` with both names encoded.
fn path_of(pos: &ExportPos) -> String {
    match pos {
        ExportPos::Fact { .. } => "facts.md".into(),
        ExportPos::Turn { agent, session, .. } => {
            format!("sessions/{}/{}.md", encode_name(agent), encode_name(session))
        }
    }
}

/// A name as 1 path segment. `a-z`, `0-9`, `-` and `_` stay. A `.` stays unless it is in the
/// leading or trailing run of dots. Every other byte becomes `%xx`, so `App` and `app`
/// differ on a case-insensitive disk. An empty name is `%`. A Windows device name gets its
/// first byte escaped. Past 200 bytes it keeps 180, then adds `~` and 16 hex chars of BLAKE3.
pub(crate) fn encode_name(name: &str) -> String {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return "%".into();
    }
    let stem = bytes.split(|byte| *byte == b'.').next().unwrap_or_default();
    let device = matches!(stem, b"con" | b"prn" | b"aux" | b"nul")
        || matches!(stem, [b'c', b'o', b'm', b'1'..=b'9'] | [b'l', b'p', b't', b'1'..=b'9']);
    let mut out = String::with_capacity(bytes.len());
    // Leading and trailing runs of `.` are escaped, so `../x` is `%2e%2e%2fx`.
    let lead = bytes.iter().take_while(|byte| **byte == b'.').count();
    let trail = bytes.len() - bytes.iter().rev().take_while(|byte| **byte == b'.').count();
    for (i, &byte) in bytes.iter().enumerate() {
        let keep = match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => !(i == 0 && device),
            b'.' => i >= lead && i < trail,
            _ => false,
        };
        match keep {
            true => out.push(char::from(byte)),
            false => out.push_str(&format!("%{byte:02x}")),
        }
    }
    if out.len() <= NAME_BYTES {
        return out;
    }
    let mut cut = 180;
    // Never split a `%xx`: a `%` in the last 2 bytes starts an escape that would be cut.
    if let Some(escape) = out[cut - 2..cut].find('%') {
        cut = cut - 2 + escape;
    }
    out.truncate(cut);
    let hash = blake3::hash(bytes).to_hex();
    out.push('~');
    out.push_str(&hash[..16]);
    out
}

/// True when `segment` could come from [`encode_name`]: 1 to 200 bytes of `a-z0-9-_.%~`, not
/// starting or ending with `.`. Each `%` is followed by 2 lowercase hex digits, or is the
/// whole segment. A `~` may appear once, followed by exactly 16 lowercase hex digits and
/// nothing else. Windows device names are rejected too.
fn valid_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if segment == "%" {
        return true;
    }
    let hex = |byte: &u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else { return false };
    if bytes.len() > NAME_BYTES || *first == b'.' || *last == b'.' {
        return false;
    }
    let stem = bytes.split(|byte| *byte == b'.').next().unwrap_or_default();
    let device = matches!(stem, b"con" | b"prn" | b"aux" | b"nul")
        || matches!(stem, [b'c', b'o', b'm', b'1'..=b'9'] | [b'l', b'p', b't', b'1'..=b'9']);
    if device {
        return false;
    }
    let mut i = 0;
    while let Some(byte) = bytes.get(i) {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => i += 1,
            b'%' if bytes.get(i + 1..i + 3).is_some_and(|pair| pair.iter().all(hex)) => i += 3,
            b'~' => {
                let tail = &bytes[i + 1..];
                return tail.len() == 16 && tail.iter().all(hex);
            }
            _ => return false,
        }
    }
    true
}

/// The 2 encoded names of a session path, or None for `facts.md`. Err for any other path.
fn split_path(path: &str) -> Result<Option<(&str, &str)>, ()> {
    if path == "facts.md" {
        return Ok(None);
    }
    let names = path.strip_prefix("sessions/").and_then(|rest| rest.strip_suffix(".md"));
    let names = names.and_then(|names| names.split_once('/'));
    match names {
        Some((agent, session)) if valid_segment(agent) && valid_segment(session) => {
            Ok(Some((agent, session)))
        }
        _ => Err(()),
    }
}

/// An entry's markdown: its heading and body. The first entry of a file carries the file
/// heading too. The same entry, `at` and forget state always give the same bytes.
fn render(entry: &ExportEntry, at: u64, first: bool) -> String {
    let mut out = String::new();
    match entry {
        ExportEntry::Fact(fact) => {
            if first {
                out.push_str(&format!("# Facts, LSN {at}\n\n"));
            }
            let (content, meta, lossy) = blobs(&fact.content, &fact.meta);
            out.push_str(&format!("## {}{}\n", quoted(&fact.key), lossy));
            let time = utc(fact.commit_time_ns);
            out.push_str(&format!("lsn {}, agent {}, {time}\n\n", fact.lsn, quoted(&fact.agent)));
            fenced(&mut out, &content, meta);
        }
        ExportEntry::Turn { agent, session, turn } => {
            if first {
                out.push_str(&format!("# {} / {}\n\n", quoted(agent), quoted(session)));
            }
            if turn.redacted {
                out.push_str(&format!("## #{} forgotten\n\n", turn.seq));
                return out;
            }
            let role = match turn.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let (content, meta, lossy) = blobs(&turn.content, &turn.meta);
            let time = utc(turn.commit_time_ns);
            let (seq, lsn) = (turn.seq, turn.lsn);
            out.push_str(&format!("## #{seq} {role}, {time}, lsn {lsn}{lossy}\n\n"));
            fenced(&mut out, &content, meta);
        }
    }
    out
}

/// A JSON string, so a newline in a name can't start a heading.
fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// Content as text and meta as 1 JSON line. Bytes that aren't UTF-8 are rendered lossy, as
/// the API's `blobs` does, and the heading says ` (lossy)`.
fn blobs(content: &[u8], meta: &[u8]) -> (String, Option<String>, &'static str) {
    let mut lossy = "";
    let mut text = |bytes: &[u8]| match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => {
            lossy = " (lossy)";
            String::from_utf8_lossy(bytes).into_owned()
        }
    };
    let meta = match serde_json::from_slice::<Value>(meta) {
        _ if meta.is_empty() => None,
        Ok(meta) => Some(meta.to_string()),
        Err(_) => Some(quoted(&text(meta))),
    };
    (text(content), meta, lossy)
}

/// Content in a fence longer than any backtick run in it, then meta, then a blank line.
fn fenced(out: &mut String, content: &str, meta: Option<String>) {
    let longest = content.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    out.push_str(&format!("{fence}\n{content}\n{fence}\n"));
    if let Some(meta) = meta {
        out.push_str(&meta);
        out.push('\n');
    }
    out.push('\n');
}

/// UTC time to the second, as `2026-11-03T13:17:17Z`.
pub(crate) fn utc(ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// What 1 page request got.
pub(crate) enum Fetched {
    /// A 200 body.
    Page(Value),
    /// A 409: the export must start again.
    Conflict,
}

/// Runs `atticdb export <dir>`: the store flags pick the store as `studio` does, and
/// `--max-bytes` sets each page's size. Exits 0, 1 when the export failed, or 2 for a bad flag.
pub fn command(dir: &str, mut flags: HashMap<String, String>) -> u8 {
    let max_bytes = match flags.remove("--max-bytes").map(|text| text.parse::<u64>()) {
        None => None,
        Some(Ok(bytes)) => Some(bytes),
        Some(Err(_)) => {
            eprintln!("atticdb: --max-bytes must be a whole number");
            return 2;
        }
    };
    let store = StoreFlags::take(&mut flags).and_then(|flags| flags.open(true));
    let Store { mut launch, token, .. } = match store {
        Ok(store) => store,
        Err(message) => {
            eprintln!("atticdb: {message}");
            return 1;
        }
    };
    let mut fetch = |target: &str| {
        let deadline = Instant::now() + PAGE_DEADLINE;
        let (mut stream, host) = launch.connect(deadline).map_err(|error| error.to_string())?;
        let auth = format!("Bearer {}", token.expose());
        let headers = [("Host", host.as_str()), ("Authorization", auth.as_str())];
        let max = api::MAX_RESPONSE_BYTES;
        let reply =
            crate::http::exchange(&mut *stream, "GET", target, &headers, &[], deadline, max);
        let reply = reply.map_err(|error| error.to_string())?;
        let body: Value = serde_json::from_slice(&reply.body).unwrap_or_default();
        match reply.status {
            200 => Ok(Fetched::Page(body)),
            409 => Ok(Fetched::Conflict),
            status => {
                let message = body["message"].as_str().or(body["error"].as_str()).unwrap_or("");
                Err(format!("the store answered {status}: {message}"))
            }
        }
    };
    match run(Path::new(dir), max_bytes, &mut fetch) {
        Ok((at, files)) => {
            println!("exported {files} files at lsn {at} to {dir}");
            0
        }
        Err(message) => {
            eprintln!("atticdb: {message}");
            1
        }
    }
}

/// Fetches every page into `<dir>.partial-<hex>`, then renames it to `dir`. A 409 deletes the
/// partial folder and starts again, at most [`RESTARTS`] times. Any other failure keeps it and
/// names it. Returns the export's LSN and its file count.
pub(crate) fn run(
    dir: &Path,
    max_bytes: Option<u64>,
    fetch: &mut dyn FnMut(&str) -> Result<Fetched, String>,
) -> Result<(u64, usize), String> {
    for _ in 0..=RESTARTS {
        let mut out = Assembly::create(dir)?;
        let mut target = target(max_bytes, None)?;
        loop {
            let page = match fetch(&target) {
                Ok(Fetched::Page(page)) => page,
                Ok(Fetched::Conflict) => {
                    out.discard();
                    break;
                }
                Err(error) => return Err(out.failed(&error)),
            };
            if let Err(error) = out.page(&page) {
                return Err(out.failed(&error));
            }
            match &page["next"] {
                Value::Null => return out.finish(dir),
                next => match self::target(max_bytes, Some(next)) {
                    Ok(next) => target = next,
                    Err(error) => return Err(out.failed(&error)),
                },
            }
        }
    }
    Err(format!("the export fell behind {} times; try again later", RESTARTS + 1))
}

/// A page request, from the previous page's `next`.
fn target(max_bytes: Option<u64>, next: Option<&Value>) -> Result<String, String> {
    let mut target = "/v1/export?format=markdown".to_owned();
    if let Some(next) = next {
        let bad = || "the store sent a bad cursor".to_owned();
        let number = |name: &str| next[name].as_u64().ok_or_else(bad);
        let text = |name: &str| next[name].as_str().map(query_value).ok_or_else(bad);
        target += &format!("&at={}", number("at")?);
        if next.get("key").is_some() {
            target += &format!("&after_key={}", text("key")?);
        } else {
            let (agent, session, seq) = (text("agent")?, text("session")?, number("seq")?);
            target += &format!("&after_agent={agent}&after_session={session}&after_seq={seq}");
        }
        target += &format!("&skip={}&offset={}", number("skip")?, number("offset")?);
    }
    if let Some(max_bytes) = max_bytes {
        target += &format!("&max_bytes={max_bytes}");
    }
    Ok(target)
}

/// Percent-encodes a query value.
fn query_value(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// The file being written: its export path, its `.part` file and the bytes written.
struct Open {
    path: String,
    part: PathBuf,
    file: File,
    written: u64,
}

/// An export being written into `<dir>.partial-<hex>`.
struct Assembly {
    root: PathBuf,
    at: Option<u64>,
    /// Every path an offset-0 chunk opened.
    seen: HashSet<String>,
    open: Option<Open>,
    /// Directories made, parents first.
    dirs: Vec<PathBuf>,
}

impl Assembly {
    /// Fails if `dir` exists. Makes `<dir>.partial-<16 hex>` beside it, mode 0700.
    fn create(dir: &Path) -> Result<Self, String> {
        if std::fs::symlink_metadata(dir).is_ok() {
            return Err(format!("{} exists; pick a new folder", dir.display()));
        }
        let name = dir.file_name().ok_or(format!("{}: not a folder name", dir.display()))?;
        let hex = crate::random_hex(8).map_err(crate::platform::no_random)?;
        let mut partial = name.to_os_string();
        partial.push(format!(".partial-{hex}"));
        let root = dir.with_file_name(partial);
        let made = DirBuilder::new().mode(0o700).create(&root);
        made.map_err(|error| format!("{}: {error}", root.display()))?;
        let dirs = vec![root.clone()];
        Ok(Self { root, at: None, seen: HashSet::new(), open: None, dirs })
    }

    /// Writes 1 page's chunks. Every path is checked before any write.
    fn page(&mut self, page: &Value) -> Result<(), String> {
        let at = page["at"].as_u64().filter(|_| page["format"] == "markdown");
        let at = at.ok_or("the store sent a page that is not a markdown export")?;
        if *self.at.get_or_insert(at) != at {
            return Err(format!("the store changed the export's lsn to {at}"));
        }
        let chunks = page["chunks"].as_array().ok_or("the store sent a page without chunks")?;
        let mut parsed = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let fields = (chunk["path"].as_str(), chunk["offset"].as_u64());
            let (Some(path), Some(offset), Some(markdown)) =
                (fields.0, fields.1, chunk["markdown"].as_str())
            else {
                return Err("the store sent a bad chunk".into());
            };
            let names = split_path(path);
            let names = names.map_err(|()| format!("the store sent a bad path {path:?}"))?;
            parsed.push((path, names, offset, markdown));
        }
        for (path, names, offset, markdown) in parsed {
            if offset == 0 {
                self.start(path, names)?;
            }
            let open =
                self.open.as_mut().filter(|open| open.path == path && open.written == offset);
            let Some(open) = open else {
                return Err(format!("the chunk at {offset} of {path} doesn't continue the file"));
            };
            let wrote = open.file.write_all(markdown.as_bytes());
            wrote.map_err(|error| format!("{}: {error}", open.part.display()))?;
            open.written += markdown.len() as u64;
        }
        Ok(())
    }

    /// Opens `<path>.part` for a file's first chunk. A path seen before fails: 2 names share
    /// it, or the store has a bug.
    fn start(&mut self, path: &str, names: Option<(&str, &str)>) -> Result<(), String> {
        if !self.seen.insert(path.to_owned()) {
            return Err(format!("2 files share the path {path}"));
        }
        self.close()?;
        let file = match names {
            None => self.root.join("facts.md"),
            Some((agent, session)) => {
                let mut dir = self.root.clone();
                for name in ["sessions", agent] {
                    dir.push(name);
                    match DirBuilder::new().mode(0o700).create(&dir) {
                        Ok(()) => self.dirs.push(dir.clone()),
                        Err(error) if error.kind() == IoKind::AlreadyExists => {}
                        Err(error) => return Err(format!("{}: {error}", dir.display())),
                    }
                }
                dir.join(format!("{session}.md"))
            }
        };
        let mut part = file.into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut options = OpenOptions::new();
        let made = options.write(true).create_new(true).mode(0o600).open(&part);
        let file = made.map_err(|error| format!("{}: {error}", part.display()))?;
        self.open = Some(Open { path: path.to_owned(), part, file, written: 0 });
        Ok(())
    }

    /// Syncs the open `.part` file and renames it to its `.md` name.
    fn close(&mut self) -> Result<(), String> {
        let Some(Open { part, file, .. }) = self.open.take() else { return Ok(()) };
        let fail = |error: std::io::Error| format!("{}: {error}", part.display());
        file.sync_all().map_err(fail)?;
        drop(file);
        std::fs::rename(&part, part.with_extension("")).map_err(fail)
    }

    /// Closes the last file, syncs every folder, then renames the partial folder to `dir`.
    fn finish(mut self, dir: &Path) -> Result<(u64, usize), String> {
        if let Err(error) = self.close() {
            return Err(self.failed(&error));
        }
        for made in self.dirs.iter().rev() {
            if let Err(error) = sync_dir(made) {
                return Err(self.failed(&format!("{}: {error}", made.display())));
            }
        }
        if std::fs::symlink_metadata(dir).is_ok() {
            return Err(self.failed(&format!("{} appeared during the export", dir.display())));
        }
        if let Err(error) = std::fs::rename(&self.root, dir) {
            return Err(self.failed(&format!("{}: {error}", dir.display())));
        }
        let parent = dir.parent().filter(|parent| !parent.as_os_str().is_empty());
        sync_dir(parent.unwrap_or(Path::new("."))).map_err(|error| error.to_string())?;
        Ok((self.at.unwrap_or_default(), self.seen.len()))
    }

    /// Deletes the partial folder, for a restart.
    fn discard(self) {
        drop(self.open);
        let _ = std::fs::remove_dir_all(&self.root);
    }

    /// `error`, plus where the partial folder stays.
    fn failed(&self, error: &str) -> String {
        format!("{error}; the partial export stays in {}", self.root.display())
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Does nothing on Windows, as the tape's `sync_dir` does: NTFS journals directory entries.
#[cfg(not(unix))]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn names_encode_to_unique_segments_the_grammar_accepts() {
        let long = "x".repeat(300);
        let long_escapes = "A".repeat(100);
        let near_cut = format!("{}A{}", "b".repeat(179), "c".repeat(30));
        let hash = |name: &str| blake3::hash(name.as_bytes()).to_hex()[..16].to_owned();
        let cases: Vec<(&str, String)> = vec![
            ("", "%".into()),
            ("app", "app".into()),
            ("App", "%41pp".into()),
            (".", "%2e".into()),
            ("..", "%2e%2e".into()),
            ("../x", "%2e%2e%2fx".into()),
            ("a.", "a%2e".into()),
            (".hidden", "%2ehidden".into()),
            ("a.b", "a.b".into()),
            ("a..", "a%2e%2e".into()),
            ("a..b", "a..b".into()),
            ("a\\b", "a%5cb".into()),
            ("%41", "%2541".into()),
            ("~x", "%7ex".into()),
            ("con", "%63on".into()),
            ("con.txt", "%63on.txt".into()),
            ("console", "console".into()),
            ("com1", "%63om1".into()),
            ("lpt9.x", "%6cpt9.x".into()),
            ("com0", "com0".into()),
            ("é", "%c3%a9".into()),
            ("a b", "a%20b".into()),
            (&long, format!("{}~{}", "x".repeat(180), hash(&long))),
            (&long_escapes, format!("{}~{}", "%41".repeat(60), hash(&long_escapes))),
            // The escape at bytes 179 to 181 is cut back whole.
            (&near_cut, format!("{}~{}", "b".repeat(179), hash(&near_cut))),
        ];
        let mut outputs = HashSet::new();
        for (name, expected) in &cases {
            let encoded = encode_name(name);
            assert_eq!(&encoded, expected, "{name:?}");
            assert!(valid_segment(&encoded), "{name:?}: {encoded} passes the grammar");
            assert!(encoded.len() <= NAME_BYTES, "{name:?}");
            assert!(outputs.insert(encoded), "{name:?}: no 2 names share an output");
            let path = format!("sessions/{}/{}.md", encode_name(name), encode_name(name));
            assert!(split_path(&path).is_ok(), "{name:?}: {path}");
        }
    }

    #[test]
    fn the_path_grammar_rejects_escapes() {
        for path in [
            "sessions/../x.md",
            "sessions/a/../x.md",
            "/abs",
            "/sessions/a/b.md",
            "sessions/a\\b/c.md",
            "sessions/.hidden/a.md",
            "sessions/x./a.md",
            "sessions/a/x..md",
            "sessions/%4/a.md",
            "sessions/%41%/a.md",
            "sessions/%4G/a.md",
            "sessions/a~1/b.md",
            "sessions/a~0123456789abcdef0/b.md",
            "sessions/A/b.md",
            "sessions/a/b/c.md",
            "sessions//b.md",
            "sessions/a/.md",
            "sessions/con/a.md",
            "facts.md.part",
            "facts",
            "",
        ] {
            assert!(split_path(path).is_err(), "{path:?} is rejected");
        }
        for path in ["facts.md", "sessions/%/%.md", "sessions/a.b/x~0123456789abcdef.md"] {
            assert!(split_path(path).is_ok(), "{path:?} is accepted");
        }
    }

    fn chunk(path: &str, offset: u64, markdown: &str) -> Value {
        json!({"path": path, "offset": offset, "markdown": markdown})
    }

    fn page(chunks: Vec<Value>, next: bool) -> Value {
        let next = next.then(|| json!({"at": 7, "key": "k", "skip": 0, "offset": 1}));
        json!({"format": "markdown", "at": 7, "chunks": chunks, "next": next})
    }

    /// Runs the assembler on `pages`, then an error once they run out.
    fn assemble(dir: &Path, pages: Vec<Value>) -> Result<(u64, usize), String> {
        let mut pages = pages.into_iter();
        let mut fetch = |_: &str| pages.next().map(Fetched::Page).ok_or("lost".to_owned());
        run(dir, None, &mut fetch)
    }

    /// The names in `dir` other than `keep`, recursively, and their bytes.
    fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(next) = todo.pop() {
            for entry in std::fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    todo.push(path);
                } else {
                    let name = path.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
                    out.push((name, std::fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn the_assembler_checks_offsets_and_leaves_no_half_export() {
        let scratch = std::env::temp_dir().join(format!("atticdb-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let session = "sessions/a/b.md";
        for (case, pages, error) in [
            (
                "a repeated offset 0",
                vec![page(vec![chunk("facts.md", 0, "x"), chunk("facts.md", 0, "y")], false)],
                "2 files share the path facts.md",
            ),
            (
                "an offset gap",
                vec![
                    page(vec![chunk("facts.md", 0, "ab")], true),
                    page(vec![chunk("facts.md", 3, "c")], false),
                ],
                "the chunk at 3 of facts.md doesn't continue the file",
            ),
            (
                "a path switch mid-file",
                vec![page(vec![chunk("facts.md", 0, "ab"), chunk(session, 2, "c")], false)],
                "the chunk at 2 of sessions/a/b.md doesn't continue the file",
            ),
            (
                "a traversal path",
                vec![page(vec![chunk("sessions/%2e%2e/../x.md", 0, "x")], false)],
                "bad path",
            ),
            (
                "an error after page 2",
                vec![
                    page(vec![chunk("facts.md", 0, "f1")], true),
                    page(vec![chunk("facts.md", 2, "f2"), chunk(session, 0, "s1")], true),
                ],
                "lost; the partial export stays in",
            ),
        ] {
            let dir = scratch.join("out");
            let message = assemble(&dir, pages).unwrap_err();
            assert!(message.contains(error), "{case}: {message}");
            assert!(std::fs::symlink_metadata(&dir).is_err(), "{case}: no <dir>");
            let partial: Vec<_> =
                std::fs::read_dir(&scratch).unwrap().map(|e| e.unwrap().path()).collect();
            assert_eq!(partial.len(), 1, "{case}: 1 partial folder");
            assert!(message.contains(&partial[0].display().to_string()), "{case}: names it");
            if case == "an error after page 2" {
                let files = listing(&partial[0]);
                let expected = vec![
                    ("facts.md".to_owned(), b"f1f2".to_vec()),
                    ("sessions/a/b.md.part".to_owned(), b"s1".to_vec()),
                ];
                assert_eq!(files, expected, "{case}: complete .md files and 1 .part file");
            }
            std::fs::remove_dir_all(&partial[0]).unwrap();
        }

        let dir = scratch.join("out");
        let pages = vec![
            page(vec![chunk("facts.md", 0, "f1")], true),
            page(vec![chunk("facts.md", 2, "f2"), chunk(session, 0, "s1")], true),
            page(vec![chunk(session, 2, "s2"), chunk("sessions/%/%.md", 0, "e")], false),
        ];
        assert_eq!(assemble(&dir, pages.clone()), Ok((7, 3)));
        let expected = vec![
            ("facts.md".to_owned(), b"f1f2".to_vec()),
            ("sessions/%/%.md".to_owned(), b"e".to_vec()),
            ("sessions/a/b.md".to_owned(), b"s1s2".to_vec()),
        ];
        assert_eq!(listing(&dir), expected);
        let message = assemble(&dir, pages).unwrap_err();
        assert!(message.contains("exists"), "an existing <dir> is refused: {message}");
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 1, "and nothing else is made");

        // A 409 deletes the partial folder and starts again from page 1, at most 3 times.
        for (case, conflicts, done) in [("1 restart", 1, true), ("4 conflicts", 4, false)] {
            let again = scratch.join("again");
            let (mut left, mut targets) = (conflicts, Vec::new());
            let mut fetch = |target: &str| {
                targets.push(target.to_owned());
                if targets.len() % 2 == 0 && left > 0 {
                    left -= 1;
                    return Ok(Fetched::Conflict);
                }
                let first = target == "/v1/export?format=markdown&max_bytes=65536";
                let chunk =
                    if first { chunk("facts.md", 0, "ab") } else { chunk("facts.md", 2, "c") };
                Ok(Fetched::Page(page(vec![chunk], first)))
            };
            let result = run(&again, Some(65536), &mut fetch);
            assert_eq!(result.is_ok(), done, "{case}: {result:?}");
            let second =
                "/v1/export?format=markdown&at=7&after_key=k&skip=0&offset=1&max_bytes=65536";
            assert_eq!(targets[1], second, "{case}: the cursor is passed back");
            let names = std::fs::read_dir(&scratch).unwrap().count();
            assert_eq!(names, if done { 2 } else { 1 }, "{case}: no partial folder is left");
            if done {
                assert_eq!(std::fs::read(again.join("facts.md")).unwrap(), b"abc", "{case}");
                std::fs::remove_dir_all(&again).unwrap();
            } else {
                assert_eq!(result.unwrap_err(), "the export fell behind 4 times; try again later");
            }
        }
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn the_size_calculator_matches_serde_json() {
        /// xorshift64, so the pages are random but the same on every run.
        struct Random(u64);
        impl Random {
            fn below(&mut self, below: u64) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 % below
            }
            fn text(&mut self, len: u64) -> String {
                const ALPHABET: [char; 18] = [
                    'a',
                    '"',
                    '\\',
                    '\n',
                    '\r',
                    '\t',
                    '\u{8}',
                    '\u{c}',
                    '\u{1}',
                    '\u{1f}',
                    '\u{7f}',
                    '\u{e9}',
                    '\u{20ac}',
                    '\u{1f600}',
                    '`',
                    '/',
                    '%',
                    ' ',
                ];
                let len = self.below(len);
                (0..len).map(|_| ALPHABET[self.below(ALPHABET.len() as u64) as usize]).collect()
            }
        }
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for round in 0..200 {
            let chunks: Vec<Chunk> = (0..random.below(4))
                .map(|_| Chunk {
                    path: format!("sessions/{}/a.md", encode_name(&random.text(30))),
                    offset: random.below(u64::MAX),
                    markdown: random.text(200),
                })
                .collect();
            let pos = match round % 3 {
                0 => ExportPos::Fact { key: random.text(1024) },
                _ => {
                    let (agent, session) = (random.text(255), random.text(255));
                    ExportPos::Turn { agent, session, seq: random.below(u64::MAX) }
                }
            };
            let (at, skip, offset) =
                (random.below(u64::MAX), random.below(1 << 20) as usize, random.below(u64::MAX));
            let cursor = Cursor { at, pos, skip, offset };
            let next = (round % 4 != 0).then_some(&cursor);
            let at = random.below(u64::MAX);
            let written = serde_json::to_vec(&body(at, &chunks, next)).unwrap().len();
            assert_eq!(body_len(at, &chunks, next), written, "round {round}");
            if let Some(cursor) = next {
                let envelope = serde_json::to_vec(&body(u64::MAX, &[], Some(cursor))).unwrap();
                assert!(envelope.len() <= RESERVE, "round {round}: the reserve covers next");
            }
        }
    }

    #[test]
    fn rendering_is_plain_and_fenced() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_700_000_000_999_999_999), "2023-11-14T22:13:20Z");
        assert_eq!(utc(951_782_400 * 1_000_000_000), "2000-02-29T00:00:00Z");
        let mut out = String::new();
        fenced(&mut out, "a ```` b", Some("{}".into()));
        assert_eq!(out, "`````\na ```` b\n`````\n{}\n\n");
        assert_eq!(
            blobs(b"\xff", b"{\"a\": 1}"),
            ("\u{fffd}".into(), Some("{\"a\":1}".into()), " (lossy)")
        );
        assert_eq!(blobs(b"x", b"a\nb"), ("x".into(), Some("\"a\\nb\"".into()), ""));
    }
}
