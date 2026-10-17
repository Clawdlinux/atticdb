//! Application hooks around the tape's durable boundary.

use crate::{Committed, Kind, PreparedBatch, Result, WriteError};
use std::sync::Arc;

/// A durable application record. Footers and redacted husks aren't application records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedRecord {
    /// Record log sequence number.
    pub lsn: u64,
    /// Transaction identifier.
    pub txn: u64,
    /// Application record category.
    pub kind: Kind,
    /// Tenant identifier.
    pub tenant_id: u32,
    /// Commit timestamp in nanoseconds since the Unix epoch.
    pub commit_time_ns: u64,
    /// Application content, without the tape's generated salt.
    pub payload: Arc<[u8]>,
}

/// The writer's next action after application preparation.
#[derive(Clone, Debug)]
pub enum Prepare {
    /// Frame and write the prepared batch.
    Write,
    /// Return an application error without writing anything.
    Reject(WriteError),
    /// Return a cached acknowledgement without writing anything.
    ///
    /// Durable replies wait for the group's sync and publication, including retries of pending writes.
    /// Fast requests always receive `Committed::Fast`, without identifiers.
    Reply(Committed),
}

/// Application state maintained in tape order.
///
/// Preparation and publication run on the writer thread, without concurrent callbacks.
/// Replay runs during open, after recovery syncs, before the writer accepts requests.
/// Keep unpublished preparation state private. Readers must only observe published state.
///
/// Preparation can reserve sequences before storage fails. Such failures poison the writer.
/// No later preparation can observe those reservations. Reopening reconstructs state through replay.
/// A rejected or cached batch must leave no new reservations behind.
///
/// Preparation may cache dedup entries for writes still pending in the current group.
/// Return their durable acknowledgement through `Prepare::Reply`; it shares the original write's sync outcome.
pub trait StateMachine: Send {
    /// Rewrites application content or short-circuits a batch before framing, once in queue order.
    fn prepare(&mut self, batch: &mut PreparedBatch) -> Prepare;

    /// Installs synced records in LSN order before durable replies. Must take O(records) work.
    ///
    /// Fast records wait for a successful sync, including automatic Fast-only flushes and clean shutdown.
    /// A failed sync never publishes its pending records.
    fn publish(&mut self, committed: &[CommittedRecord]);

    /// Rebuilds state from each durable record strictly above the configured replay watermark.
    ///
    /// An error aborts open. Replay must perform the same visible installs as publication.
    fn replay(&mut self, record: &CommittedRecord) -> Result<()>;
}

/// Tape-only behavior without application state.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopStateMachine;

impl StateMachine for NoopStateMachine {
    fn prepare(&mut self, _batch: &mut PreparedBatch) -> Prepare {
        Prepare::Write
    }

    fn publish(&mut self, _committed: &[CommittedRecord]) {}

    fn replay(&mut self, _record: &CommittedRecord) -> Result<()> {
        Ok(())
    }
}
