//! Footer-atomic replay and durable-boundary validation.

use crate::dir::{corruption, read_exact, DataDir, Handle};
use crate::frame::{self, Digest, Footer, Frame, Kind, FLAG_HUSK};
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

/// Results of opening and repairing an unsynced tail.
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    /// Highest complete record LSN after recovery and sync.
    pub durable_lsn: u64,
    /// Discarded LSN range and removed capacity, including preallocated tails.
    pub truncated: Option<(RangeInclusive<u64>, u64)>,
    /// Number of retained manifest segments.
    pub segments: usize,
    /// Number of replayed records, excluding footers.
    pub records: usize,
}

/// Rebuilt writer state. Only complete transactions contribute records or chain heads.
#[derive(Debug, Default)]
pub struct Recovered {
    /// Recovery results exposed by the deterministic driver.
    pub report: RecoveryReport,
    pub(crate) index: BTreeMap<u64, (u64, u64)>,
    pub(crate) chains: BTreeMap<u32, Digest>,
    pub(crate) txn: u64,
    pub(crate) offset: u64,
}

fn verify(records: &[(Frame, u64)], footer: &Frame, state: &Recovered) -> Result<()> {
    let end = Footer::decode(&footer.payload).map_err(|e| Error::InvalidFrame(e.to_string()))?;
    if records.is_empty()
        || end.txn != footer.header.txn
        || end.txn <= state.txn
        || end.record_count as usize != records.len()
        || footer.header.flags != 0
        || footer.header.tenant_id != 0
        || footer.header.content_digest != [0; 32]
    {
        return Err(Error::InvalidFrame("invalid transaction footer".into()));
    }
    let mut lsn = state.report.durable_lsn;
    let mut heads = BTreeMap::new();
    let mut digests = Vec::new();
    for (record, _) in records {
        let h = &record.header;
        if h.txn != end.txn || h.lsn <= lsn || h.commit_time_ns != footer.header.commit_time_ns {
            return Err(Error::InvalidFrame("transaction or LSN order".into()));
        }
        lsn = h.lsn;
        if h.flags & FLAG_HUSK == 0 {
            let salt: [u8; 16] = record.payload[..16]
                .try_into()
                .map_err(|_| Error::InvalidFrame("missing salt".into()))?;
            if frame::content_digest(&salt, &record.payload[16..]) != h.content_digest {
                return Err(Error::InvalidFrame("content digest mismatch".into()));
            }
        }
        if h.kind != Kind::Cache {
            let prev = heads
                .get(&h.tenant_id)
                .or_else(|| state.chains.get(&h.tenant_id))
                .copied()
                .unwrap_or([0; 32]);
            if prev != h.tenant_prev_chain {
                return Err(Error::InvalidFrame("tenant chain mismatch".into()));
            }
            heads.insert(h.tenant_id, frame::chain_next(&prev, h, &h.content_digest));
        }
        digests.push(h.content_digest);
    }
    if footer.header.lsn != lsn || frame::batch_digest(&digests) != end.batch_digest {
        return Err(Error::InvalidFrame("footer digest or LSN mismatch".into()));
    }
    Ok(())
}

fn zero_tail(dir: &DataDir, file: &Handle, from: u64, size: u64) -> Result<()> {
    dir.io.set_len(file.id, size)?;
    let zeros = [0; 64 * 1024];
    let mut offset = from;
    while offset < size {
        let count = (size - offset).min(zeros.len() as u64) as usize;
        dir.io.write_at(file.id, offset, &zeros[..count])?;
        offset += count as u64;
    }
    Ok(())
}

fn last_decodable_lsn(mut bytes: &[u8]) -> u64 {
    let mut last = 0;
    while bytes.len() >= 8 {
        let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if !(frame::HEADER_LEN..=frame::MAX_FRAME_SIZE).contains(&len) || len > bytes.len() {
            break;
        }
        if let Ok((record, _)) = frame::decode(&bytes[..len]) {
            last = last.max(record.header.lsn);
        }
        bytes = &bytes[len..];
    }
    last
}

/// Scans prepared segments, verifies chains, and syncs repaired tails before returning.
pub fn recover(dir: &mut DataDir, size: u64) -> Result<Recovered> {
    dir.check_segment_size(size)?;
    let mut state = Recovered::default();
    let segments = dir.segments().to_vec();
    let mut retained = Vec::new();
    let mut trimming = false;
    let mut first_lost = 0;
    let mut last_lost = 0;
    let mut cleared = 0;
    for (position, &segment) in segments.iter().enumerate() {
        let file = match Handle::open(dir.io.clone(), &dir.segment_path(segment)) {
            Ok(file) => file,
            Err(Error::Io(std::io::ErrorKind::NotFound, _)) => {
                if state.report.durable_lsn < dir.superblock() {
                    return Err(corruption(
                        state.report.durable_lsn.saturating_add(1),
                        segment,
                        0,
                        "missing segment",
                    ));
                }
                if !trimming {
                    first_lost = state.report.durable_lsn.saturating_add(1);
                }
                trimming = true;
                last_lost = last_lost.max(first_lost);
                continue;
            }
            Err(error) => return Err(error),
        };
        let len = dir.io.len(file.id)?;
        if trimming {
            if len <= size {
                let mut bytes = vec![0; len as usize];
                read_exact(dir.io.as_ref(), file.id, 0, &mut bytes)?;
                last_lost = last_lost.max(last_decodable_lsn(&bytes));
            }
            cleared += len;
            continue;
        }
        retained.push(segment);
        if len > size || usize::try_from(len).is_err() {
            return Err(corruption(
                state.report.durable_lsn.saturating_add(1),
                segment,
                0,
                "invalid segment length",
            ));
        }
        let mut bytes = vec![0; len as usize];
        read_exact(dir.io.as_ref(), file.id, 0, &mut bytes)?;
        let mut offset = 0;
        let mut rewrite_from = (state.report.durable_lsn >= dir.superblock()).then_some(0);
        let mut pending = Vec::<(Frame, u64)>::new();
        loop {
            let tail = &bytes[offset..];
            let empty = tail.iter().take(8).all(|&byte| byte == 0);
            let decoded = if empty && pending.is_empty() && tail.iter().all(|&b| b == 0) {
                // Zero padding between complete transactions is a valid roll boundary.
                if (offset == 0 || position + 1 == segments.len())
                    && state.report.durable_lsn < dir.superblock()
                {
                    return Err(corruption(
                        state.report.durable_lsn.saturating_add(1),
                        segment,
                        offset as u64,
                        "early end of data",
                    ));
                }
                if state.report.durable_lsn < dir.superblock()
                    && tail.len() > frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 44
                {
                    return Err(corruption(
                        state.report.durable_lsn.saturating_add(1),
                        segment,
                        offset as u64,
                        "oversized zero padding",
                    ));
                }
                let oversized = tail.len() > frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 44;
                if len == size && !((offset == 0 || oversized) && position + 1 < segments.len()) {
                    break;
                }
                Err("end of tape or short segment".to_owned())
            } else {
                frame::decode(tail).map_err(|e| e.to_string())
            };
            let result = decoded.and_then(|(record, used)| {
                if record.header.kind == Kind::Footer {
                    verify(&pending, &record, &state).map_err(|e| e.to_string())?;
                    if rewrite_from.is_none() && record.header.lsn >= dir.superblock() {
                        rewrite_from = Some(if record.header.lsn == dir.superblock() {
                            offset + used
                        } else {
                            pending[0].1 as usize
                        });
                    }
                    for (record, off) in pending.drain(..) {
                        let h = &record.header;
                        state.index.insert(h.lsn, (segment, off));
                        if h.kind != Kind::Cache {
                            state.chains.insert(
                                h.tenant_id,
                                frame::chain_next(&h.tenant_prev_chain, h, &h.content_digest),
                            );
                        }
                        state.report.durable_lsn = h.lsn;
                        state.report.records += 1;
                    }
                    state.txn = record.header.txn;
                } else {
                    let start = pending.first().map_or(offset, |(_, off)| *off as usize);
                    if pending.len() == 1024
                        || offset + used - start + frame::HEADER_LEN + 44 > frame::MAX_FRAME_SIZE
                    {
                        return Err("transaction exceeds batch limits".into());
                    }
                    pending.push((record, offset as u64));
                }
                offset += used;
                Ok(())
            });
            if let Err(reason) = result {
                let start = pending.first().map_or(offset as u64, |(_, off)| *off);
                let next = state.report.durable_lsn.saturating_add(1);
                if state.report.durable_lsn < dir.superblock() {
                    return Err(corruption(next, segment, start, reason));
                }
                first_lost =
                    pending.first().map_or(next, |(record, _)| record.header.lsn.max(next));
                last_lost = last_decodable_lsn(&bytes[start as usize..]).max(first_lost);
                zero_tail(dir, &file, start, size)?;
                cleared += size - start;
                state.offset = start;
                trimming = true;
                break;
            }
        }
        if !trimming {
            state.offset = offset as u64;
        }
        let rewrite_end = if trimming { state.offset as usize } else { bytes.len() };
        if let Some(from) = rewrite_from.filter(|&from| from < rewrite_end) {
            // A failed writeback can leave clean, readable cache bytes that another sync won't persist.
            dir.io.write_at(file.id, from as u64, &bytes[from..rewrite_end])?;
        }
        dir.io.sync_data(file.id)?;
    }
    if state.report.durable_lsn < dir.superblock() {
        return Err(corruption(
            state.report.durable_lsn.saturating_add(1),
            0,
            0,
            "durable boundary not reached",
        ));
    }
    if retained != segments {
        dir.install_manifest(&retained)?;
        for &segment in &segments[retained.len()..] {
            let path = dir.segment_path(segment);
            if dir.io.exists(&path)? {
                dir.io.remove(&path)?;
            }
        }
    }
    dir.io.sync_dir(&dir.path)?;
    if trimming {
        state.report.truncated = Some((first_lost..=last_lost.max(first_lost), cleared));
    }
    state.report.segments = retained.len();
    Ok(state)
}
