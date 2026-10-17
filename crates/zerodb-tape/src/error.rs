//! Shared tape errors.

use std::fmt;

/// A tape operation failed.
#[derive(Debug)]
pub enum Error {
    /// An I/O operation failed with its original error kind.
    Io(std::io::ErrorKind, String),
    /// Durable tape bytes failed validation.
    CorruptionDetected {
        /// First affected log sequence number.
        lsn: u64,
        /// Segment identifier.
        segment: u64,
        /// Byte offset within the segment.
        offset: u64,
        /// Validation failure.
        reason: String,
    },
    /// Another opener holds the data directory lock.
    DataDirLocked {
        /// Text from the lock file.
        holder: String,
    },
    /// A failed sync prevents further writes.
    Poisoned,
    /// The disk reserve or available space is exhausted.
    DiskFull,
    /// The batch exceeds its record or byte limit.
    BatchTooLarge,
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind(), error.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind, message) => write!(f, "{kind:?}: {message}"),
            Self::CorruptionDetected { lsn, segment, offset, reason } => {
                write!(f, "corruption at LSN {lsn}, segment {segment}, offset {offset}: {reason}")
            }
            Self::DataDirLocked { holder } => write!(f, "data directory locked by {holder}"),
            Self::Poisoned => f.write_str("tape poisoned by a failed sync"),
            Self::DiskFull => f.write_str("disk full"),
            Self::BatchTooLarge => f.write_str("batch too large"),
        }
    }
}

impl std::error::Error for Error {}

/// A tape operation result.
pub type Result<T> = std::result::Result<T, Error>;
