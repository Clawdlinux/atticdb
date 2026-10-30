//! Version 1 fact payloads. Attribution does not change the tenant-scoped key.

use crate::turn::{bytes, CodecError, Cursor};

/// A fact revision. One fact record per write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fact {
    /// Tenant-scoped UTF-8 key, at most 1024 bytes.
    pub key: String,
    /// Opaque content, at most 4 MiB.
    pub content: Vec<u8>,
    /// Opaque metadata, at most 64 KiB.
    pub meta: Vec<u8>,
    /// Attribution only. At most 255 UTF-8 bytes.
    pub agent: String,
    /// At most 1024 source LSNs. Tenant checks belong to the engine.
    pub source_refs: Vec<u64>,
    /// Optional retry key, at most 128 UTF-8 bytes.
    pub idem_key: Option<String>,
    /// Marks a deleted revision.
    pub tombstone: bool,
}

/// Encodes an exact, length-limited fact payload.
pub fn encode(fact: &Fact) -> Result<Vec<u8>, CodecError> {
    if fact.key.len() > 1024
        || fact.content.len() > 4 * 1024 * 1024
        || fact.meta.len() > 65536
        || fact.agent.len() > 255
        || fact.source_refs.len() > 1024
        || fact.idem_key.as_ref().is_some_and(|key| key.len() > 128)
    {
        return Err(CodecError);
    }
    let mut out = vec![1];
    bytes(&mut out, fact.key.as_bytes());
    bytes(&mut out, &fact.content);
    bytes(&mut out, &fact.meta);
    bytes(&mut out, fact.agent.as_bytes());
    out.extend_from_slice(&(fact.source_refs.len() as u32).to_le_bytes());
    for lsn in &fact.source_refs {
        out.extend_from_slice(&lsn.to_le_bytes());
    }
    put_key(&mut out, fact.idem_key.as_deref());
    out.push(u8::from(fact.tombstone));
    Ok(out)
}

pub(crate) fn put_key(out: &mut Vec<u8>, key: Option<&str>) {
    out.push(u8::from(key.is_some()));
    if let Some(key) = key {
        bytes(out, key.as_bytes());
    }
}

pub(crate) fn get_key(cur: &mut Cursor<'_>) -> Result<Option<String>, CodecError> {
    match cur.byte()? {
        0 => Ok(None),
        1 => Ok(Some(cur.string(128)?)),
        _ => Err(CodecError),
    }
}

/// Decodes a fact, rejecting unknown versions, tags and trailing bytes.
pub fn decode(payload: &[u8]) -> Result<Fact, CodecError> {
    let mut cur = Cursor(payload);
    if cur.byte()? != 1 {
        return Err(CodecError);
    }
    let key = cur.string(1024)?;
    let content = cur.blob(4 * 1024 * 1024)?.to_vec();
    let meta = cur.blob(65536)?.to_vec();
    let agent = cur.string(255)?;
    let source_refs = lsns(&mut cur)?;
    let idem_key = get_key(&mut cur)?;
    let tombstone = flag(&mut cur)?;
    cur.end()?;
    Ok(Fact { key, content, meta, agent, source_refs, idem_key, tombstone })
}

/// Decodes the key, retry key and tombstone bit without copying content.
pub(crate) fn header(payload: &[u8]) -> Result<(String, Option<String>, bool), CodecError> {
    let mut cur = Cursor(payload);
    if cur.byte()? != 1 {
        return Err(CodecError);
    }
    let key = cur.string(1024)?;
    for max in [4 * 1024 * 1024, 65536, 255] {
        cur.blob(max)?;
    }
    lsns(&mut cur)?;
    let idem_key = get_key(&mut cur)?;
    let tombstone = flag(&mut cur)?;
    cur.end()?;
    Ok((key, idem_key, tombstone))
}

fn flag(cur: &mut Cursor<'_>) -> Result<bool, CodecError> {
    match cur.byte()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CodecError),
    }
}

pub(crate) fn lsns(cur: &mut Cursor<'_>) -> Result<Vec<u64>, CodecError> {
    let count = cur.u32()? as usize;
    if count > 1024 {
        return Err(CodecError);
    }
    (0..count).map(|_| cur.u64()).collect()
}
