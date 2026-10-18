use std::fmt;
use zerodb_tape::{Error, Outcome, WriteError};

/// The reason an engine operation failed.
#[derive(Clone, Debug)]
pub enum ErrorKind {
    /// Input violates the engine's format or batch rules.
    InvalidInput,
    /// A retry key was already used with different content.
    IdempotencyConflict,
    /// A configured memory limit prevented a write.
    ResourceExhausted(&'static str),
    /// Stored engine data failed validation.
    CorruptIndex,
    /// The writer or its shared state needs reopening.
    Poisoned,
    /// A tape or checkpoint operation failed.
    Storage(Error),
}

/// An engine failure and its persistence outcome.
#[derive(Clone, Debug)]
pub struct EngineError {
    /// Failure category.
    pub kind: ErrorKind,
    /// What this operation proves about persistence.
    pub outcome: Outcome,
}

impl EngineError {
    pub(crate) fn rejected(kind: ErrorKind) -> Self {
        Self { kind, outcome: Outcome::NotCommitted }
    }
    pub(crate) fn write(error: WriteError) -> Self {
        let kind = match error.kind {
            Error::Rejected("idempotency") => ErrorKind::IdempotencyConflict,
            Error::Rejected("invalid_input") => ErrorKind::InvalidInput,
            Error::Rejected("corrupt_index") => ErrorKind::CorruptIndex,
            Error::Poisoned => ErrorKind::Poisoned,
            Error::Rejected(resource) => ErrorKind::ResourceExhausted(resource),
            error => ErrorKind::Storage(error),
        };
        Self { kind, outcome: error.outcome }
    }
}
impl From<Error> for EngineError {
    fn from(error: Error) -> Self {
        Self::write(WriteError { kind: error, outcome: Outcome::NotCommitted })
    }
}
impl From<crate::turn::CodecError> for EngineError {
    fn from(_: crate::turn::CodecError) -> Self {
        Self::rejected(ErrorKind::InvalidInput)
    }
}
impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} ({:?})", self.kind, self.outcome)
    }
}
impl std::error::Error for EngineError {}

pub(crate) type Result<T> = std::result::Result<T, EngineError>;

pub(crate) fn lock<T>(value: &std::sync::Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    value.lock().map_err(|_| EngineError::from(Error::Poisoned))
}
