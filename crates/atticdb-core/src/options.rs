use atticdb_tape::TapeOptions;
use std::time::Duration;

/// When the engine compresses turn content. See [`Compression`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionMode {
    /// New stores compress. An existing store compresses only once its MANIFEST is version 3.
    Auto,
    /// Compress. An existing store's MANIFEST is raised to version 3 after recovery, so v0.2.0
    /// and earlier refuse it from then on.
    On,
    /// Never compress new turns. Compressed turns stay readable.
    Off,
}

/// Turn compression at write time: raw deflate on the caller's thread, after the retry digest.
#[derive(Clone, Copy, Debug)]
pub struct Compression {
    /// When to compress.
    pub mode: CompressionMode,
    /// Deflate level, 0 to 10.
    pub level: u8,
    /// Smallest content compressed, in bytes. Shorter turns stay plain.
    pub min_content: usize,
}

impl Default for Compression {
    fn default() -> Self {
        // Search reads candidates from the word index, so compression costs it little (D96).
        Self { mode: CompressionMode::Auto, level: 3, min_content: 256 }
    }
}

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
    /// Bytes published between incremental checkpoints. Turns count uncompressed, so this bounds
    /// replay work after a crash.
    pub checkpoint_bytes: u64,
    /// Maximum idle interval before the checkpoint worker wakes.
    pub checkpoint_interval: Duration,
    /// Maximum fact keys across tenants, at least 1. Tombstones keep their slots.
    pub max_facts: usize,
    /// Revisions reported per fact key, 1 to 4,096. The tape keeps every revision.
    pub fact_history: usize,
    /// Turn compression.
    pub compression: Compression,
    /// Memory budget of the word index behind `search`, in bytes. 0 turns it off, so search
    /// walks every record. Past the budget it stops growing and search walks newer records.
    pub search_index_bytes: usize,
    /// Newest records `search` walks 1 by 1 before it asks the word index. A common word fills
    /// its page there, cheaper than a dictionary scan.
    pub search_walk_first: u64,
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
            compression: Compression::default(),
            // About 14% of the turn text on real transcripts, so this covers about 3.5 GiB.
            search_index_bytes: 512 * 1024 * 1024,
            // On 512 MiB of real transcripts, 1,024 records cost about 1 ms to walk, against
            // about 5 ms per word for the dictionary scan.
            search_walk_first: 1024,
        }
    }
}
