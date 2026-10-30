//! The AtticDB server, its HTTP client half, the MCP adapter and the studio bridge.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod api;
pub mod auth;
pub mod config;
pub mod connect;
pub mod doctor;
#[cfg(feature = "fault-inject")]
pub mod fault;
pub mod home;
pub mod hook;
pub mod http;
pub mod jsonedit;
pub mod launch;
pub mod legacy;
pub mod listen;
pub mod mcp;
pub mod ops;
pub mod outcome;
mod redact;
pub mod serve;
pub mod setup;
pub mod studio;

use std::io::Read;

/// The version this binary reports. Release builds set `ATTICDB_RELEASE_VERSION` from the git
/// tag at compile time. Main is protected, so no bot commits a bump to Cargo.toml. Any other
/// build reports the Cargo.toml version plus `+dev`.
pub const VERSION: &str = match option_env!("ATTICDB_RELEASE_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => concat!(env!("CARGO_PKG_VERSION"), "+dev"),
};

/// Spawns `command`, retrying up to 10 times, 20 ms apart, while Linux says the program is
/// busy (ETXTBSY). That happens while the file is open for writing, for example while it is
/// being updated, or in tests that just wrote a fake program while another thread forked.
pub(crate) fn spawn(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    for _ in 1..10 {
        match command.spawn() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            spawned => return spawned,
        }
    }
    command.spawn()
}

/// Reads `bytes` random bytes from `/dev/urandom` and returns them as lowercase hex.
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buf = vec![0; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|byte| format!("{byte:02x}")).collect())
}
