//! Durable session history built on the zerodb tape.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod checkpoint;
mod dedup;
mod error;
mod options;
mod session;
mod state;
mod tenant;
pub mod turn;

pub use error::{EngineError, ErrorKind};
pub use options::EngineOptions;
pub use zerodb_tape::Outcome;
