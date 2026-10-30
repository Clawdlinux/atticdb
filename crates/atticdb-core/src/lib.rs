//! Durable session history built on the AtticDB tape.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod checkpoint;
mod dedup;
mod engine;
mod error;
pub mod fact;
mod facts;
pub mod forget;
mod machine;
mod options;
mod session;
mod state;
mod tenant;
pub mod turn;

pub use atticdb_tape::Outcome;
pub use engine::{
    Appended, Engine, FactHistory, FactPage, FactQuery, FactWritten, Health, Hit, Metrics, NewFact,
    Record, RecordKind, RecordPage, RecordQuery, Revision, Scope, SearchPage, SearchQuery,
    SessionInfo, SessionPage, SessionQuery, StoredFact, Turn, Window, WindowQuery,
};
#[cfg(feature = "sim")]
pub use engine::{SimAppend, SimFact};
pub use error::{EngineError, ErrorKind};
pub use options::EngineOptions;
pub use turn::{NewTurn, Role};
