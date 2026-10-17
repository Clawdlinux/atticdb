//! Tape storage primitives for zerodb.

#![forbid(unsafe_code)]

pub mod crc32c;
mod error;
pub mod frame;
pub mod io;
pub mod simdisk;

pub use error::{Error, Result};
