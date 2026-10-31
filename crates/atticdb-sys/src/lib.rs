//! Safe wrappers over the few Win32 calls AtticDB needs.
//!
//! This is the only workspace crate that may use `unsafe`. Every other crate forbids it.
//! On targets other than Windows the crate is empty.

// Win32 calls are foreign functions, so they need unsafe. Each block states why it is sound.
#![allow(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{fill_random, free_space, rename_write_through, try_lock_past_eof, unlock_past_eof};
