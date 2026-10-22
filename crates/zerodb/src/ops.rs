//! Operator commands against a running server.

use crate::config::Token;
use crate::launch::{self, Launch, Url};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

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
    match body.as_ref().map(Value::as_object) {
        Ok(Some(fields)) => {
            for (key, value) in fields {
                match value {
                    Value::String(text) => println!("{key}={text}"),
                    other => println!("{key}={other}"),
                }
            }
            0
        }
        Ok(None) => {
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
                    let (mut stream, host) = Launch::new(Some(url), None, None)
                        .connect(deadline())
                        .map_err(|error| error.to_string())?;
                    let auth = format!("Bearer {}", token.expose());
                    let headers = [
                        ("Host", host.as_str()),
                        ("Authorization", &auth),
                        ("Connection", "keep-alive"),
                    ];
                    let mut samples = Vec::with_capacity(turns / writers / batch);
                    for i in 0..turns / writers / batch {
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
    println!("writers={writers}\nbatch={batch}\nsize={size}\nturns={done}\nseconds={seconds:.3}");
    println!("turns_per_sec={:.1}", done as f64 / seconds);
    for (name, p) in [("p50_us", 0.5), ("p99_us", 0.99), ("p999_us", 0.999), ("max_us", 1.0)] {
        println!("{name}={:.1}", us(p));
    }
    0
}
