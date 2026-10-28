//! Operator commands against a running server or a copy of a stopped data directory.

use crate::config::Token;
use crate::launch::{self, Launch, Url};
use crate::serve::EXIT_LOCKED;
use serde_json::{json, Value};
use std::fs::{DirBuilder, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zerodb_core::{Engine, EngineOptions, ErrorKind};
use zerodb_tape::{StdIo, SystemClock, Tape, TapeOptions};

/// Prints `GET /v1/stats` as `key=value` lines. Never starts a server: that would hide an outage.
pub fn stats(socket: Option<&Path>, data_dir: Option<&Path>) -> u8 {
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = match (socket, data_dir) {
        (Some(path), _) => {
            launch::connect_unix(path, deadline).map_err(|e| format!("{}: {e}", path.display()))
        }
        (None, Some(dir)) => launch::discover(dir, deadline),
        (None, None) => Err("stats needs a socket or a data directory".into()),
    };
    let response = stream.and_then(|mut stream| {
        let host = [("Host", "localhost")];
        crate::http::exchange(&mut stream, "GET", "/v1/stats", &host, &[], deadline, 1 << 20)
            .map_err(|error| error.to_string())
    });
    let body = response.and_then(|response| match response.status {
        200 => serde_json::from_slice::<Value>(&response.body).map_err(|e| e.to_string()),
        status => Err(format!("status {status}: {}", String::from_utf8_lossy(&response.body))),
    });
    match body {
        Ok(fields) if fields.is_object() => {
            print_fields(&fields);
            0
        }
        Ok(_) => {
            eprintln!("zerodb stats: response is not a JSON object");
            1
        }
        Err(error) => {
            eprintln!("zerodb stats: {error}");
            1
        }
    }
}

/// Closed-loop append load, the way agents write: each writer waits for its ack before the
/// next batch, on one keep-alive connection. Prints `key=value` lines, latency per ack.
pub fn load(
    url: Url,
    token: &Token,
    writers: usize,
    turns: usize,
    batch: usize,
    size: usize,
    think: Duration,
) -> u8 {
    // A reused run id would replay cached acks and count them as writes.
    let run = match crate::random_hex(4) {
        Ok(run) => run,
        Err(error) => {
            eprintln!("zerodb load: cannot read /dev/urandom: {error}");
            return 1;
        }
    };
    let turn = json!({"role": "user", "content": "x".repeat(size)});
    let started = Instant::now();
    let results: Vec<Result<Vec<Duration>, String>> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..writers)
            .map(|writer| {
                let (url, turn, run) = (url.clone(), &turn, &run);
                scope.spawn(move || {
                    let deadline = || Instant::now() + Duration::from_secs(30);
                    let (mut stream, host) = Launch::new(Some(url), None, None, None)
                        .connect(deadline())
                        .map_err(|error| error.to_string())?;
                    let auth = format!("Bearer {}", token.expose());
                    let headers = [
                        ("Host", host.as_str()),
                        ("Authorization", &auth),
                        ("Connection", "keep-alive"),
                    ];
                    let mut samples = Vec::with_capacity(turns / writers / batch);
                    let mut rng = u64::from_str_radix(run, 16).unwrap_or(0) ^ writer as u64;
                    for i in 0..turns / writers / batch {
                        // Uniform in [0, 2 * think): an agent's model call between writes.
                        if !think.is_zero() {
                            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                            std::thread::sleep(
                                think.mul_f64((rng >> 11) as f64 / (1u64 << 52) as f64),
                            );
                        }
                        let body = json!({"agent": "load", "session": format!("load-{writer}"),
                            "idem_key": format!("{run}-{writer}-{i}"), "turns": vec![turn; batch]});
                        let body = body.to_string();
                        let sent = Instant::now();
                        let response = crate::http::exchange(
                            &mut *stream,
                            "POST",
                            "/v1/append",
                            &headers,
                            body.as_bytes(),
                            deadline(),
                            1 << 20,
                        )
                        .map_err(|error| error.to_string())?;
                        samples.push(sent.elapsed());
                        if response.status != 200 {
                            let text = String::from_utf8_lossy(&response.body);
                            return Err(format!("status {}: {text}", response.status));
                        }
                    }
                    Ok(samples)
                })
            })
            .collect();
        threads.into_iter().map(|thread| thread.join().unwrap_or(Err("panicked".into()))).collect()
    });
    let seconds = started.elapsed().as_secs_f64();
    let mut samples = Vec::new();
    for result in results {
        match result {
            Ok(mut writer) => samples.append(&mut writer),
            Err(error) => {
                eprintln!("zerodb load: {error}");
                return 1;
            }
        }
    }
    samples.sort_unstable();
    let done = samples.len() * batch;
    // Nearest rank, as G1 reports.
    let us = |p: f64| {
        samples[((samples.len() as f64 * p).ceil() as usize).max(1) - 1].as_secs_f64() * 1e6
    };
    let think = think.as_millis();
    println!("writers={writers}\nbatch={batch}\nsize={size}\nthink_ms={think}\nturns={done}");
    println!("seconds={seconds:.3}");
    println!("turns_per_sec={:.1}", done as f64 / seconds);
    for (name, p) in [("p50_us", 0.5), ("p99_us", 0.99), ("p999_us", 0.999), ("max_us", 1.0)] {
        println!("{name}={:.1}", us(p));
    }
    0
}

/// Prints object fields as `key=value` lines, sorted by key.
fn print_fields(fields: &Value) {
    for (key, value) in fields.as_object().into_iter().flatten() {
        match value {
            Value::String(text) => println!("{key}={text}"),
            other => println!("{key}={other}"),
        }
    }
}

/// `zerodb verify`: the real engine open on a copy, then the `GET /v1/stats` fields.
pub fn verify(dir: &Path, scratch: Option<&Path>) -> u8 {
    on_copy(dir, scratch, |copy| {
        let engine =
            Engine::open(copy, EngineOptions::default()).map_err(|error| match error.kind {
                ErrorKind::Storage(error) => error.to_string(),
                _ => error.to_string(),
            })?;
        let fields = crate::api::stats_fields(&engine).ok_or("engine poisoned")?;
        print_fields(&fields);
        // Load logged the reason. Replay hid it, so a clean open is not a clean directory.
        if fields["checkpoint_warnings"] != 0 {
            return Err("a checkpoint was rejected and the tape replayed".into());
        }
        Ok(())
    })
}

/// `zerodb dump`: one line per durable record from `from` on, at most `limit`. Never content.
pub fn dump(dir: &Path, scratch: Option<&Path>, from: u64, limit: usize) -> u8 {
    on_copy(dir, scratch, |copy| {
        let (io, clock) = (Arc::new(StdIo::new()), Arc::new(SystemClock));
        let (tape, _) = Tape::open(io, clock, copy, TapeOptions::default())
            .map_err(|error| error.to_string())?;
        let mut out = std::io::stdout().lock();
        for record in tape.scan(from).take(limit) {
            let r = record.map_err(|error| error.to_string())?;
            // Not `println!`: it panics when the reader, such as `head`, exits first.
            writeln!(
                out,
                "lsn={} txn={} kind={:?} tenant={} bytes={} commit_time_ns={}",
                r.lsn,
                r.txn,
                r.kind,
                r.tenant_id,
                r.payload.len(),
                r.commit_time_ns
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    })
}

/// A scratch copy, removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("zerodb: cannot remove {}: {error}", self.0.display());
        }
    }
}

/// Runs `inspect` on a copy of `dir` and removes the copy. Returns the exit code.
/// Opening repairs a directory, so the original is only locked and read.
fn on_copy(
    dir: &Path,
    scratch: Option<&Path>,
    inspect: impl FnOnce(&Path) -> Result<(), String>,
) -> u8 {
    let result = copy_locked(dir, scratch).and_then(|copy| inspect(&copy.0).map_err(|e| (1, e)));
    result.map_or_else(
        |(code, message)| {
            eprintln!("zerodb: {}: {message}", dir.display());
            code
        },
        |()| 0,
    )
}

/// Copies the regular files of `dir` into a new mode 0700 directory under its `LOCK`.
fn copy_locked(dir: &Path, scratch: Option<&Path>) -> Result<Scratch, (u8, String)> {
    let fail = |error: std::io::Error| (1, error.to_string());
    let mut options = OpenOptions::new();
    let mut lock = match crate::config::open_regular(options.read(true), &dir.join("LOCK")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err((1, "not a data directory".into()))
        }
        opened => opened.map_err(fail)?,
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut holder = String::new();
            let _ = lock.read_to_string(&mut holder);
            return Err((EXIT_LOCKED, format!("locked by {}", holder.trim())));
        }
        Err(TryLockError::Error(error)) => return Err(fail(error)),
    }
    // Canonical, so `.` or a trailing slash cannot put the copy inside `dir`.
    let dir = std::fs::canonicalize(dir).map_err(fail)?;
    let parent = match scratch {
        Some(scratch) => std::fs::canonicalize(scratch).map_err(fail)?,
        None => dir.parent().ok_or((1, "no parent for the copy".into()))?.to_owned(),
    };
    // A copy inside the source would add and remove entries in it.
    if parent.starts_with(&dir) {
        return Err((2, "--scratch must be outside the data directory".into()));
    }
    let mut name = dir.file_name().unwrap_or_default().to_owned();
    name.push(format!(".verify-{}", crate::random_hex(4).map_err(fail)?));
    let path = parent.join(name);
    let named = |error| (1, format!("{}: {error}", path.display()));
    DirBuilder::new().mode(0o700).create(&path).map_err(named)?;
    let copy = Scratch(path);
    for entry in std::fs::read_dir(&dir).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let kind = entry.file_type().map_err(fail)?;
        // The server's socket stays behind. Anything else would make an incomplete copy.
        if kind.is_socket() {
            continue;
        }
        if !kind.is_file() {
            return Err((1, format!("{}: not a regular file", entry.path().display())));
        }
        {
            let to = copy.0.join(entry.file_name());
            std::fs::copy(entry.path(), &to).map_err(fail)?;
            // Opening needs write access, and a backup can be read-only.
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600)).map_err(fail)?;
        }
    }
    Ok(copy)
}
