//! Tape storage primitives for zerodb.

#![forbid(unsafe_code)]

pub mod crc32c;
mod error;
pub mod io;

pub use error::{Error, Result};
