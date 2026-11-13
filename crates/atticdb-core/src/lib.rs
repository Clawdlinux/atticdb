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
mod forgets;
mod hot_cache;
mod machine;
mod options;
mod rank;
mod search_index;
mod session;
mod state;
mod tenant;
pub mod turn;

pub use atticdb_tape::Outcome;
pub use engine::{
    Appended, Engine, ExportEntry, ExportPage, ExportPos, ExportQuery, FactHistory, FactPage,
    FactQuery, FactWritten, Forgotten, Health, Hit, Metrics, NewFact, Record, RecordKind,
    RecordPage, RecordQuery, Revision, Scope, SearchOrder, SearchPage, SearchQuery, SessionInfo,
    SessionPage, SessionQuery, StoredFact, Turn, Window, WindowQuery,
};
#[cfg(feature = "sim")]
pub use engine::{SimAppend, SimFact, SimForget};
pub use error::{EngineError, ErrorKind};
pub use options::{Compression, CompressionMode, EngineOptions};
pub use turn::{NewTurn, Role};
