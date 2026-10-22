//! The zerodb server, its HTTP client half, and the MCP adapter.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod api;
pub mod auth;
pub mod config;
#[cfg(feature = "fault-inject")]
pub mod fault;
pub mod http;
pub mod launch;
pub mod listen;
pub mod mcp;
pub mod ops;
pub mod outcome;
pub mod serve;

use std::io::Read;

/// Reads `bytes` random bytes from `/dev/urandom` and returns them as lowercase hex.
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buf = vec![0; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|byte| format!("{byte:02x}")).collect())
}
