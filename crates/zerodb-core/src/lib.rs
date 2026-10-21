//! Durable session history built on the zerodb tape.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod checkpoint;
mod dedup;
mod engine;
mod error;
pub mod fact;
pub mod forget;
mod machine;
mod options;
mod session;
mod state;
mod tenant;
pub mod turn;

#[cfg(feature = "sim")]
pub use engine::SimAppend;
pub use engine::{Appended, Engine, Health, Metrics, Scope, Turn, Window, WindowQuery};
pub use error::{EngineError, ErrorKind};
pub use options::EngineOptions;
pub use turn::{NewTurn, Role};
pub use zerodb_tape::Outcome;
