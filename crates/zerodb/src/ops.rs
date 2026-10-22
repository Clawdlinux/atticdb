//! Operator commands that read a running server.

use crate::launch;
use serde_json::Value;
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
