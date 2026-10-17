//! Durable session history built on the zerodb tape.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod tenant;
pub mod turn;

pub use error::{EngineError, ErrorKind};
pub use zerodb_tape::Outcome;
