//! Footer-atomic replay and durable-boundary validation.

use crate::dir::{corruption, read_exact, DataDir, Handle};
use crate::frame::{self, Footer, Frame, Kind};
use crate::lsn_index::LsnIndex;
use crate::{Error, Result};
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
    /// Explicit operator cut, including a resumed cut after a crash.
    pub cut_lsn: Option<u64>,
    /// An operator cut permits reuse of dropped LSNs, including acknowledged LSNs.
    pub lsns_may_be_reused: bool,
    /// Bytes cleared or removed, including preallocated tails without decodable records.
    pub dropped_bytes: u64,
}

/// Rebuilt writer state. Only complete transactions contribute records.
#[derive(Debug, Default)]
pub struct Recovered {
    /// Recovery results exposed by the deterministic driver.
    pub report: RecoveryReport,
    pub(crate) index: Option<LsnIndex>,
    pub(crate) txn: u64,
    pub(crate) commit_time_ns: u64,
    pub(crate) synced_lsn: u64,
    pub(crate) offset: u64,
}

fn verify(records: &[(Frame, u64)], footer: &Frame, state: &Recovered, segment: u64) -> Result<()> {
    let end = Footer::decode(&footer.payload).map_err(|e| Error::InvalidFrame(e.to_string()))?;
    if records.is_empty()
        || end.txn != footer.header.txn
        || state.txn.checked_add(1) != Some(end.txn)
        || state.report.durable_lsn.checked_add(1) != Some(end.first_lsn)
        || end.record_count as usize != records.len()
        || footer.header.flags != 0
        || footer.header.tenant_id != 0
        || end.synced_lsn < state.synced_lsn
        || end.synced_lsn >= end.first_lsn
        || footer.header.commit_time_ns <= state.commit_time_ns
    {
        return Err(Error::InvalidFrame("invalid transaction footer".into()));
    }
    let mut lsn = state.report.durable_lsn;
    let mut crc = crate::crc32c::Crc32c::new();
    for (record, offset) in records {
        let h = &record.header;
        if h.txn != end.txn
            || lsn.checked_add(1) != Some(h.lsn)
            || h.commit_time_ns != footer.header.commit_time_ns
        {
            return Err(Error::InvalidFrame("transaction or LSN order".into()));
        }
        lsn = h.lsn;
        crc.update(&frame::encode(record, segment, *offset)?[..4]);
    }
    if footer.header.lsn != lsn || crc.finish() != end.records_crc {
        return Err(Error::InvalidFrame("footer CRC or LSN mismatch".into()));
    }
    Ok(())
}

fn next_candidate(bytes: &[u8], mut offset: usize) -> Option<usize> {
    while offset.checked_add(frame::HEADER_LEN)? <= bytes.len() {
        if bytes[offset..offset + 8].iter().all(|&byte| byte == 0) {
            let nonzero = bytes[offset + 8..].iter().position(|&byte| byte != 0)?;
            offset += (nonzero + 1).max(1);
            continue;
        }
        if bytes[offset + 9] == frame::VERSION && bytes[offset + 10..offset + 12] == [0, 0] {
            return Some(offset);
        }
        offset += 1;
    }
    None
}

fn proving_footer(bytes: &[u8], segment: u64, from: usize, e: u64, known: &Recovered) -> bool {
    let mut offset = from;
    let mut previous_synced = known.synced_lsn;
    while let Some(start) = next_candidate(bytes, offset) {
        offset = start + 1;
        let Ok((first, used)) = frame::decode(&bytes[start..], segment, start as u64) else {
            continue;
        };
        if first.header.kind == Kind::Footer
            || first.header.lsn <= e
            || first.header.txn <= known.txn
        {
            continue;
        }
        let state = Recovered {
            txn: first.header.txn - 1,
            report: RecoveryReport {
                durable_lsn: first.header.lsn - 1,
                ..RecoveryReport::default()
            },
            commit_time_ns: known.commit_time_ns,
            synced_lsn: previous_synced,
            ..Recovered::default()
        };
        let mut records = vec![(first, start as u64)];
        let mut end = start + used;
        while end - start <= frame::MAX_FRAME_SIZE && records.len() <= 1024 {
            let Ok((record, used)) = frame::decode(&bytes[end..], segment, end as u64) else {
                break;
            };
            if end + used - start > frame::MAX_FRAME_SIZE {
                break;
            }
            if record.header.kind == Kind::Footer {
                if verify(&records, &record, &state, segment).is_ok() {
                    let footer = Footer::decode(&record.payload).ok();
                    if let Some(footer) = footer {
                        previous_synced = footer.synced_lsn;
                        if footer.synced_lsn >= e {
                            return true;
                        }
                    }
                }
                break;
            }
            records.push((record, end as u64));
            end += used;
        }
    }
    false
}

fn lookahead(
    dir: &DataDir,
    size: u64,
    position: usize,
    offset: u64,
    e: u64,
    known: &Recovered,
) -> Result<bool> {
    for (index, &segment) in dir.segments().iter().enumerate().skip(position) {
        let file = match Handle::open(dir.io.clone(), &dir.segment_path(segment)) {
            Ok(file) => file,
            Err(Error::Io(std::io::ErrorKind::NotFound, _)) => continue,
            Err(error) => return Err(error),
        };
        let len = dir.io.len(file.id)?.min(size);
        let mut bytes = vec![0; len as usize];
        read_exact(dir.io.as_ref(), file.id, 0, &mut bytes)?;
        let from = if index == position { offset as usize } else { 0 };
        if proving_footer(&bytes, segment, from, e, known) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn expected_bad_lsn(state: &Recovered, pending: &[(Frame, u64)], tail: &[u8]) -> u64 {
    let next = state.report.durable_lsn.saturating_add(pending.len() as u64).saturating_add(1);
    if let Some((last, _)) = pending.last() {
        // A damaged kind or zeroed footer cannot advance its expected LSN past the pending records.
        if tail.get(8) == Some(&(Kind::Footer as u8))
            || tail.iter().take(8).all(|&byte| byte == 0)
            || tail.get(20..28) == Some(last.header.lsn.to_le_bytes().as_slice())
        {
            return last.header.lsn;
        }
    }
    next
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

fn last_decodable_lsn(bytes: &[u8], segment: u64, base: u64) -> u64 {
    let mut last = 0;
    let mut offset = 0;
    while let Some(start) = next_candidate(bytes, offset) {
        offset = start + 1;
        if let Ok((record, used)) = frame::decode(&bytes[start..], segment, base + start as u64) {
            last = last.max(record.header.lsn);
            offset = start + used;
        }
    }
    last
}

struct Cut {
    retained: Vec<u64>,
    offset: u64,
    records: usize,
}

fn cut_prefix(dir: &DataDir, size: u64, cut: u64) -> Result<Cut> {
    if cut == 0 {
        return Ok(Cut { retained: Vec::new(), offset: 0, records: 0 });
    }
    let mut state = Recovered::default();
    let mut retained = Vec::new();
    for &segment in dir.segments() {
        let expected = state.report.durable_lsn.saturating_add(1);
        let file = Handle::open(dir.io.clone(), &dir.segment_path(segment))
            .map_err(|error| corruption(expected, segment, 0, error))?;
        let len = dir.io.len(file.id)?;
        if len > size {
            return Err(corruption(expected, segment, 0, "invalid segment length"));
        }
        let mut bytes = vec![0; len as usize];
        read_exact(dir.io.as_ref(), file.id, 0, &mut bytes)
            .map_err(|error| corruption(expected, segment, 0, error))?;
        retained.push(segment);
        let mut offset = 0;
        let mut pending = Vec::new();
        while offset < bytes.len() {
            if pending.is_empty() && bytes[offset..].iter().all(|&byte| byte == 0) {
                if offset == 0
                    || bytes.len() - offset > frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 32
                {
                    return Err(corruption(
                        expected,
                        segment,
                        offset as u64,
                        "early end or oversized padding",
                    ));
                }
                break;
            }
            let expected =
                state.report.durable_lsn.saturating_add(pending.len() as u64).saturating_add(1);
            let bad_lsn = expected_bad_lsn(&state, &pending, &bytes[offset..]);
            let (record, used) = frame::decode(&bytes[offset..], segment, offset as u64)
                .map_err(|error| corruption(bad_lsn, segment, offset as u64, error))?;
            if record.header.kind == Kind::Footer {
                verify(&pending, &record, &state, segment)
                    .map_err(|error| corruption(bad_lsn, segment, offset as u64, error))?;
                let last = record.header.lsn;
                if last >= cut {
                    if last != cut {
                        return Err(Error::InvalidFrame(format!(
                            "cut LSN {cut} is inside a transaction; valid cut points: {} and {last}",
                            state.report.durable_lsn,
                        )));
                    }
                    return Ok(Cut {
                        retained,
                        offset: (offset + used) as u64,
                        records: state.report.records + pending.len(),
                    });
                }
                state.txn = record.header.txn;
                state.commit_time_ns = record.header.commit_time_ns;
                state.synced_lsn = Footer::decode(&record.payload)
                    .map_err(|error| corruption(last, segment, offset as u64, error))?
                    .synced_lsn;
                state.report.durable_lsn = last;
                state.report.records += pending.len();
                pending.clear();
            } else {
                if record.header.lsn != expected
                    || state.txn.checked_add(1) != Some(record.header.txn)
                    || pending.first().is_some_and(|(first, _)| {
                        first.header.commit_time_ns != record.header.commit_time_ns
                    })
                    || record.header.commit_time_ns <= state.commit_time_ns
                {
                    return Err(corruption(
                        expected,
                        segment,
                        offset as u64,
                        "transaction or LSN order",
                    ));
                }
                let start = pending.first().map_or(offset, |(_, off)| *off as usize);
                if pending.len() == 1024
                    || offset + used - start + frame::HEADER_LEN + 32 > frame::MAX_FRAME_SIZE
                {
                    return Err(corruption(
                        expected,
                        segment,
                        offset as u64,
                        "transaction exceeds batch limits",
                    ));
                }
                pending.push((record, offset as u64));
            }
            offset += used;
        }
        if !pending.is_empty() || len < size {
            return Err(corruption(
                state.report.durable_lsn.saturating_add(pending.len() as u64).saturating_add(1),
                segment,
                offset as u64,
                "early end of data",
            ));
        }
    }
    Err(Error::InvalidFrame(format!(
        "cut LSN {cut} exceeds last valid cut point {}",
        state.report.durable_lsn
    )))
}

fn finish_cut(dir: &mut DataDir, size: u64, values: &[u64]) -> Result<RecoveryReport> {
    let [lsn, last_lost, cleared] = values else {
        return Err(corruption(0, 0, 0, "invalid truncate intent length"));
    };
    let prefix = cut_prefix(dir, size, *lsn)?;
    let segments = dir.segments().to_vec();
    for (position, &segment) in segments.iter().enumerate() {
        if position + 1 < prefix.retained.len() {
            continue;
        }
        let file = match Handle::open(dir.io.clone(), &dir.segment_path(segment)) {
            Ok(file) => file,
            Err(Error::Io(std::io::ErrorKind::NotFound, _))
                if position >= prefix.retained.len() =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        let from = if position < prefix.retained.len() { prefix.offset } else { 0 };
        zero_tail(dir, &file, from, size)?;
        dir.io.sync_data(file.id)?;
    }
    // Persist the zeroed data first. Then lower the superblock. The durable intent bridges a crash between them.
    dir.install_superblock(*lsn)?;
    dir.install_manifest(&prefix.retained)?;
    for &segment in &segments[prefix.retained.len()..] {
        let path = dir.segment_path(segment);
        if dir.io.exists(&path)? {
            dir.io.remove(&path)?;
        }
    }
    dir.io.sync_dir(&dir.path)?;
    dir.io.remove(&dir.path.join("TRUNCATE"))?;
    dir.io.sync_dir(&dir.path)?;
    let report = RecoveryReport {
        durable_lsn: *lsn,
        truncated: lsn
            .checked_add(1)
            .filter(|first| first <= last_lost)
            .map(|first| (first..=*last_lost, *cleared)),
        segments: prefix.retained.len(),
        records: prefix.records,
        cut_lsn: Some(*lsn),
        lsns_may_be_reused: true,
        dropped_bytes: *cleared,
    };
    eprintln!("zerodb: operator cut LSN {lsn}, dropped LSNs {:?}, dropped bytes {cleared}; dropped LSNs may be reused", report.truncated.as_ref().map(|(range, _)| range));
    Ok(report)
}

fn resume_truncate(dir: &mut DataDir, size: u64) -> Result<Option<RecoveryReport>> {
    let path = dir.path.join("TRUNCATE");
    if !dir.io.exists(&path)? {
        return Ok(None);
    }
    let values = crate::dir::read(dir.io.clone(), &path, b"ZDBCUT01", 1)?;
    finish_cut(dir, size, &values).map(Some)
}

pub(crate) fn truncate_at(dir: &mut DataDir, size: u64, lsn: u64) -> Result<RecoveryReport> {
    resume_truncate(dir, size)?;
    let prefix = cut_prefix(dir, size, lsn)?;
    let mut last_lost = lsn;
    let mut cleared = 0u64;
    for (position, &segment) in dir.segments().iter().enumerate() {
        if position + 1 < prefix.retained.len() {
            continue;
        }
        let file = match Handle::open(dir.io.clone(), &dir.segment_path(segment)) {
            Ok(file) => file,
            Err(Error::Io(std::io::ErrorKind::NotFound, _)) => continue,
            Err(error) => return Err(error),
        };
        let len = dir.io.len(file.id)?;
        if len > size {
            return Err(corruption(lsn.saturating_add(1), segment, 0, "invalid segment length"));
        }
        let from = if position < prefix.retained.len() { prefix.offset.min(len) } else { 0 };
        let mut bytes = vec![0; (len - from) as usize];
        read_exact(dir.io.as_ref(), file.id, from, &mut bytes)?;
        last_lost = last_lost.max(last_decodable_lsn(&bytes, segment, from));
        cleared = cleared.saturating_add(size - from);
    }
    let values = [lsn, last_lost, cleared];
    let intent = crate::dir::encode(b"ZDBCUT01", 1, &values);
    crate::dir::atomic_install(dir.io.clone(), &dir.path, "TRUNCATE", &intent)?;
    finish_cut(dir, size, &values)
}

/// Scans prepared segments and syncs repaired tails before returning.
#[cfg(any(test, feature = "sim"))]
pub fn recover(dir: &mut DataDir, size: u64) -> Result<Recovered> {
    recover_with_budget(dir, size, crate::lsn_index::DEFAULT_BUDGET)
}

pub(crate) fn recover_with_budget(
    dir: &mut DataDir,
    size: u64,
    budget: usize,
) -> Result<Recovered> {
    dir.check_segment_size(size)?;
    let resumed = resume_truncate(dir, size)?;
    let mut state = Recovered {
        index: Some(LsnIndex::new(dir.io.clone(), &dir.path, budget)?),
        ..Recovered::default()
    };
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
                let e = state.report.durable_lsn.saturating_add(1);
                if state.report.durable_lsn < dir.superblock()
                    || lookahead(dir, size, position + 1, 0, e, &state)?
                {
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
                last_lost = last_lost.max(last_decodable_lsn(&bytes, segment, 0));
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
        let mut touched = false;
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
                    && tail.len() > frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 32
                {
                    return Err(corruption(
                        state.report.durable_lsn.saturating_add(1),
                        segment,
                        offset as u64,
                        "oversized zero padding",
                    ));
                }
                let oversized = tail.len() > frame::MAX_FRAME_SIZE + frame::HEADER_LEN + 32;
                if len == size && !((offset == 0 || oversized) && position + 1 < segments.len()) {
                    break;
                }
                Err("end of tape or short segment".to_owned())
            } else {
                match frame::decode(tail, segment, offset as u64) {
                    Err(frame::DecodeError::UnknownVersion) => {
                        return Err(Error::InvalidFrame(
                            "unsupported frame version: expected v2".into(),
                        ));
                    }
                    result => result.map_err(|e| e.to_string()),
                }
            };
            let bad_lsn = expected_bad_lsn(&state, &pending, tail);
            let mut index_error = None;
            let result = decoded.and_then(|(record, used)| {
                if record.header.kind == Kind::Footer {
                    verify(&pending, &record, &state, segment).map_err(|e| e.to_string())?;
                    if rewrite_from.is_none() && record.header.lsn >= dir.superblock() {
                        rewrite_from = Some(if record.header.lsn == dir.superblock() {
                            offset + used
                        } else {
                            pending[0].1 as usize
                        });
                    }
                    for (record, off) in pending.drain(..) {
                        let h = &record.header;
                        if let Err(error) = state
                            .index
                            .as_mut()
                            .ok_or(Error::Poisoned)
                            .and_then(|index| index.insert(h.lsn, segment, off))
                        {
                            index_error = Some(error);
                            return Err("LSN index update failed".into());
                        }
                        state.report.durable_lsn = h.lsn;
                        state.report.records += 1;
                    }
                    state.txn = record.header.txn;
                    state.commit_time_ns = record.header.commit_time_ns;
                    state.synced_lsn =
                        Footer::decode(&record.payload).map_err(|e| e.to_string())?.synced_lsn;
                } else {
                    let expected = state
                        .report
                        .durable_lsn
                        .saturating_add(pending.len() as u64)
                        .saturating_add(1);
                    if record.header.lsn != expected
                        || state.txn.checked_add(1) != Some(record.header.txn)
                        || pending.first().is_some_and(|(first, _)| {
                            first.header.commit_time_ns != record.header.commit_time_ns
                        })
                        || record.header.commit_time_ns <= state.commit_time_ns
                    {
                        return Err("transaction, timestamp or LSN order".into());
                    }
                    let start = pending.first().map_or(offset, |(_, off)| *off as usize);
                    if pending.len() == 1024
                        || offset + used - start + frame::HEADER_LEN + 32 > frame::MAX_FRAME_SIZE
                    {
                        return Err("transaction exceeds batch limits".into());
                    }
                    pending.push((record, offset as u64));
                }
                offset += used;
                Ok(())
            });
            if let Some(error) = index_error {
                return Err(error);
            }
            if let Err(reason) = result {
                let start = pending.first().map_or(offset as u64, |(_, off)| *off);
                let next = state.report.durable_lsn.saturating_add(1);
                if state.report.durable_lsn < dir.superblock()
                    || lookahead(dir, size, position, offset as u64, bad_lsn, &state)?
                {
                    return Err(corruption(bad_lsn, segment, offset as u64, reason));
                }
                first_lost =
                    pending.first().map_or(next, |(record, _)| record.header.lsn.max(next));
                last_lost =
                    last_decodable_lsn(&bytes[start as usize..], segment, start).max(first_lost);
                zero_tail(dir, &file, start, size)?;
                touched = true;
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
            touched = true;
        }
        if touched {
            dir.io.sync_data(file.id)?;
        }
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
        state.report.dropped_bytes = cleared;
        eprintln!(
            "zerodb: truncated torn tail LSNs {first_lost}..={}, dropped bytes {cleared}",
            last_lost.max(first_lost)
        );
    }
    state.report.segments = retained.len();
    if let Some(report) = resumed {
        state.report.cut_lsn = report.cut_lsn;
        state.report.lsns_may_be_reused = true;
        state.report.truncated = report.truncated;
        state.report.dropped_bytes = report.dropped_bytes;
    }
    Ok(state)
}
