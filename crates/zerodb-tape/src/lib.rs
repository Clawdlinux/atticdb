//! Tape storage primitives for zerodb.

#![forbid(unsafe_code)]

#[cfg(any(test, feature = "sim"))]
extern crate self as zerodb_tape;

pub mod crc32c;
pub mod dir;
mod error;
pub mod frame;
pub mod io;
pub mod recovery;
pub mod segment;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
pub mod simdisk;
pub mod writer;

pub use error::{Error, Result};
pub use recovery::RecoveryReport;
pub use writer::{Committed, Durability, Options, Outcome, RecordInput, Tape, WriteError};
