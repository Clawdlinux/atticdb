use atticdb_tape::TapeOptions;
use std::time::Duration;

/// Storage and memory bounds. Limits apply before reserving a write.
#[derive(Clone, Debug)]
pub struct EngineOptions {
    /// Tape segment and sync configuration. The engine owns the replay watermark.
    pub tape: TapeOptions,
    /// Total resident session-slot capacity, in bytes.
    pub hot_page_bytes: usize,
    /// Maximum session directory entries.
    pub max_sessions: usize,
    /// Maximum page directory entries, including cold pages.
    pub max_pages: usize,
    /// Maximum tenants. Cannot exceed 10,000.
    pub max_tenants: usize,
    /// Retained retry keys per tenant.
    pub dedup_per_tenant: usize,
    /// Retained retry keys across tenants.
    pub dedup_global: usize,
    /// Tape bytes between incremental checkpoints.
    pub checkpoint_bytes: u64,
    /// Maximum idle interval before the checkpoint worker wakes.
    pub checkpoint_interval: Duration,
    /// Maximum fact keys across tenants, at least 1. Tombstones keep their slots.
    pub max_facts: usize,
    /// Revisions reported per fact key, 1 to 4,096. The tape keeps every revision.
    pub fact_history: usize,
}
impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            tape: TapeOptions::default(),
            hot_page_bytes: 256 * 1024 * 1024,
            max_sessions: 1_000_000,
            max_pages: 8_000_000,
            max_tenants: 10_000,
            dedup_per_tenant: 1_000_000,
            dedup_global: 8_000_000,
            checkpoint_bytes: 64 * 1024 * 1024,
            checkpoint_interval: Duration::from_secs(60),
            max_facts: 100_000,
            fact_history: 16,
        }
    }
}
