//! Tape storage primitives for zerodb.
//!
//! Import storage types from the crate root. Implementation modules aren't public.
//! Deterministic driver access requires the `sim` feature.
//!
//! ```compile_fail
//! use zerodb_tape::writer::WriterCore;
//! ```
//!
//! ```compile_fail
//! use zerodb_tape::dir::DataDir;
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub(crate) mod crc32c;
pub(crate) mod dir;
mod error;
pub(crate) mod frame;
pub(crate) mod io;
mod lsn_index;
pub(crate) mod recovery;
pub(crate) mod segment;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
pub(crate) mod simdisk;
mod state_machine;
pub(crate) mod writer;

pub use error::{Error, Result};
pub use frame::{Digest, Frame, Header, Kind};
pub use io::{Clock, FileId, Io, LockGuard, StdIo, SystemClock};
pub use lsn_index::LsnIndexStats;
pub use recovery::RecoveryReport;
pub use simdisk::SimDisk;
pub use state_machine::{CommittedRecord, NoopStateMachine, Prepare, StateMachine};
pub use writer::{
    Committed, Durability, Outcome, PreparedBatch, RecordInput, Tape, TapeOptions, WriteError,
    WriteResult,
};
