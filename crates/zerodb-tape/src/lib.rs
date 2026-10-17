//! Tape storage primitives for zerodb.

#![forbid(unsafe_code)]

pub mod crc32c;
pub mod dir;
mod error;
pub mod frame;
pub mod io;
pub mod segment;
pub mod simdisk;

pub use error::{Error, Result};
