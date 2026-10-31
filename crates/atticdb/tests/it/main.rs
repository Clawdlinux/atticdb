#![allow(clippy::unwrap_used)]

// These need the Unix socket, Unix modes or Unix tools. Windows runs `portable`, `apps` and
// `cli`.
#[cfg(unix)]
mod api;
mod apps;
mod cli;
#[cfg(unix)]
mod connect;
#[cfg(unix)]
mod durability;
#[cfg(unix)]
mod hook;
#[cfg(unix)]
mod listen;
#[cfg(unix)]
mod mcp;
mod portable;
#[cfg(unix)]
mod setup;
#[cfg(unix)]
mod studio;
#[cfg(unix)]
mod support;
