use crate::dedup::Dedup;
use crate::session::{Index, Key};
use crate::tenant::Registry;
use crate::{EngineError, EngineOptions};
use std::collections::BTreeSet;

#[derive(Debug)]
pub(crate) struct State {
    pub(crate) tenants: Registry,
    pub(crate) index: Index,
    pub(crate) dedup: Dedup,
    pub(crate) dirty: BTreeSet<(Key, usize)>,
    pub(crate) published_lsn: u64,
    pub(crate) prepared_lsn: u64,
    pub(crate) txn: u64,
    pub(crate) bytes: u64,
    pub(crate) checkpoint_bytes: u64,
    pub(crate) watermark: u64,
    pub(crate) chain: Vec<String>,
    pub(crate) replayed: u64,
    pub(crate) warnings: u64,
    pub(crate) replay_failed: bool,
    pub(crate) failure: Option<EngineError>,
    pub(crate) notify: Option<std::sync::mpsc::SyncSender<crate::checkpoint::Command>>,
}
impl State {
    pub(crate) fn new(opts: &EngineOptions) -> Self {
        Self {
            tenants: Registry::default(),
            index: Index::new(opts.hot_page_bytes, opts.max_sessions, opts.max_pages),
            dedup: Dedup::new(opts.dedup_per_tenant, opts.dedup_global),
            dirty: BTreeSet::new(),
            published_lsn: 0,
            prepared_lsn: 0,
            txn: 0,
            bytes: 0,
            checkpoint_bytes: 0,
            watermark: 0,
            chain: Vec::new(),
            replayed: 0,
            warnings: 0,
            replay_failed: false,
            failure: None,
            notify: None,
        }
    }
}
