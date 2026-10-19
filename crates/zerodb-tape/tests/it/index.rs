use std::sync::Arc;
use zerodb_tape::sim::{PreparedBatch, SimClock, WriteRequest, WriterCore};
use zerodb_tape::{Durability, Kind, RecordInput, SimDisk, TapeOptions};

#[test]
fn paged_offsets_cross_segments_recover_and_reload_within_budget() {
    let disk = Arc::new(SimDisk::new(11));
    let opts =
        TapeOptions { segment_size: 4096, lsn_index_bytes: 12 * 1024, ..TapeOptions::default() };
    let mut expected = Vec::new();
    let (mut core, _) =
        WriterCore::open(disk.clone(), Arc::new(SimClock::default()), "db", opts.clone()).unwrap();
    for group in 0..30u64 {
        let records = (0..64u64)
            .map(|index| {
                let content = (group * 64 + index).to_le_bytes().to_vec();
                expected.push(content.clone());
                RecordInput { kind: Kind::Turn, tenant_id: 1, content }
            })
            .collect();
        let batch = PreparedBatch::new(records, Durability::Durable).unwrap();
        let (request, reply) = WriteRequest::new(batch);
        core.enqueue(request);
        core.step();
        reply.recv().unwrap().unwrap();
        assert!(core.index_stats().unwrap().resident_bytes <= opts.lsn_index_bytes);
    }
    let evictions = core.index_stats().unwrap().page_evictions;
    assert!(evictions > 0);
    assert!(core.index_stats().unwrap().dirty_page_evictions > 0);
    disk.enospc_after_bytes(0).unwrap();
    assert!(core.read(1).is_err());
    assert_eq!(core.index_stats().unwrap().page_evictions, evictions);
    disk.enospc_after_bytes(usize::MAX).unwrap();
    assert_eq!(core.read(1920).unwrap().unwrap().payload, expected[1919]);
    let reads_before = core.index_stats().unwrap().page_reloads;
    for lsn in [1, 1920, 256, 1025, 512, 1919, 1] {
        assert_eq!(core.read(lsn).unwrap().unwrap().payload, expected[lsn as usize - 1]);
        assert!(core.index_stats().unwrap().resident_bytes <= opts.lsn_index_bytes);
    }
    assert!(core.index_stats().unwrap().page_reloads > reads_before);
    assert!(core.read(0).unwrap().is_none());
    assert!(core.read(1921).unwrap().is_none());
    core.shutdown().unwrap();
    drop(core);
    let (core, report) = WriterCore::open(
        Arc::new(disk.crash().unwrap()),
        Arc::new(SimClock::default()),
        "db",
        opts.clone(),
    )
    .unwrap();
    assert_eq!(report.durable_lsn, 1920);
    assert!(report.segments > 1);
    for (index, content) in expected.iter().enumerate().rev() {
        assert_eq!(core.read(index as u64 + 1).unwrap().unwrap().payload, *content);
        assert!(core.index_stats().unwrap().resident_bytes <= opts.lsn_index_bytes);
    }
}

#[test]
fn an_index_budget_smaller_than_one_page_is_rejected() {
    let result = WriterCore::open(
        Arc::new(SimDisk::new(12)),
        Arc::new(SimClock::default()),
        "db",
        TapeOptions { lsn_index_bytes: 1, ..TapeOptions::default() },
    );
    assert!(result.unwrap_err().to_string().contains("index"));
}
