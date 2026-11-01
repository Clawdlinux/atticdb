//! Turn payloads. Length prefixes are little-endian u32 values.
//!
//! Version 1: version, agent, session, seq, role, meta, content, retry key.
//! Version 2: the same, except content is `raw_len: u32` and a raw deflate stream. Only
//! [`compress`] writes it, and only when the stream is smaller. Stores holding it have
//! MANIFEST version 3, which v0.2.0 and earlier refuse to open.

use std::borrow::Cow;
use std::fmt;

const PLAIN: u8 = 1;
const DEFLATE: u8 = 2;
const MAX_CONTENT: usize = 4 * 1024 * 1024;

/// The speaker's role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Role {
    /// Instructions for the session.
    System,
    /// Input from a person.
    User,
    /// Agent output.
    Assistant,
    /// Tool output.
    Tool,
}

/// A turn before the writer assigns its sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewTurn {
    /// Speaker role.
    pub role: Role,
    /// Opaque content, at most 4 MiB.
    pub content: Vec<u8>,
    /// Opaque metadata, at most 64 KiB.
    pub meta: Vec<u8>,
}

/// A decoded tape payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedTurn {
    /// Agent identifier, at most 255 UTF-8 bytes.
    pub agent_id: String,
    /// Session identifier, at most 255 UTF-8 bytes.
    pub session_id: String,
    /// Writer-assigned sequence, starting at 1.
    pub seq: u64,
    /// Turn body.
    pub turn: NewTurn,
    /// Batch retry key, at most 128 UTF-8 bytes.
    pub idem_key: Option<String>,
}

/// A malformed or oversized turn payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodecError;

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid turn payload or field limit")
    }
}
impl std::error::Error for CodecError {}

pub(crate) fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
}

pub(crate) struct Cursor<'a>(pub(crate) &'a [u8]);
impl<'a> Cursor<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let value = self.0.get(..n).ok_or(CodecError)?;
        self.0 = &self.0[n..];
        Ok(value)
    }
    pub(crate) fn byte(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| CodecError)?))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| CodecError)?))
    }
    pub(crate) fn blob(&mut self, max: usize) -> Result<&'a [u8], CodecError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(CodecError);
        }
        self.take(n)
    }
    pub(crate) fn string(&mut self, max: usize) -> Result<String, CodecError> {
        String::from_utf8(self.blob(max)?.to_vec()).map_err(|_| CodecError)
    }
    pub(crate) fn end(self) -> Result<(), CodecError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(CodecError)
        }
    }
}

/// Encodes a turn. Callers pass sequence 0 before preparation.
///
/// The optional key uses a presence byte, followed by its length-prefixed UTF-8 bytes.
pub fn encode(
    agent: &str,
    session: &str,
    seq: u64,
    turn: &NewTurn,
    idem_key: Option<&str>,
) -> Result<Vec<u8>, CodecError> {
    if agent.len() > 255
        || session.len() > 255
        || turn.meta.len() > 65536
        || turn.content.len() > 4 * 1024 * 1024
        || idem_key.is_some_and(|key| key.len() > 128)
    {
        return Err(CodecError);
    }
    // Size the payload once. Growing it from one byte reallocates about ten times per turn, and
    // under glibc those cross-tcache reallocs contend on arena locks with the writer and readers.
    let len = 1
        + (4 + agent.len())
        + (4 + session.len())
        + 8
        + 1
        + (4 + turn.meta.len())
        + (4 + turn.content.len())
        + 1
        + idem_key.map_or(0, |key| 4 + key.len());
    let mut out = Vec::with_capacity(len);
    out.push(PLAIN);
    bytes(&mut out, agent.as_bytes());
    bytes(&mut out, session.as_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(turn.role as u8);
    bytes(&mut out, &turn.meta);
    bytes(&mut out, &turn.content);
    out.push(u8::from(idem_key.is_some()));
    if let Some(key) = idem_key {
        bytes(&mut out, key.as_bytes());
    }
    debug_assert_eq!(out.len(), len);
    Ok(out)
}

/// Decodes an exact payload of either version, rejecting trailing bytes and unknown tags or
/// versions. Inflates version 2 content.
pub fn decode(payload: &[u8]) -> Result<DecodedTurn, CodecError> {
    let parsed = decode_ref(payload)?;
    Ok(DecodedTurn {
        agent_id: parsed.agent_id.to_owned(),
        session_id: parsed.session_id.to_owned(),
        seq: parsed.seq,
        turn: NewTurn {
            role: parsed.role,
            content: parsed.content.bytes()?.into_owned(),
            meta: parsed.meta.to_vec(),
        },
        idem_key: parsed.idem_key.map(str::to_owned),
    })
}

/// Rewrites a version 1 payload as version 2 when its content is at least `min_content` bytes
/// and the deflate stream at `level` (0 to 10) is smaller. Otherwise returns it unchanged.
pub fn compress(payload: Vec<u8>, level: u8, min_content: usize) -> Result<Vec<u8>, CodecError> {
    let parsed = decode_ref(&payload)?;
    let Content::Plain(content) = parsed.content else { return Err(CodecError) };
    if content.len() < min_content.max(1) {
        return Ok(payload);
    }
    let stream = miniz_oxide::deflate::compress_to_vec(content, level);
    // Version 2 adds `raw_len`, so it must save more than those 4 bytes.
    if stream.len() + 4 >= content.len() {
        return Ok(payload);
    }
    let tail = tail_len(parsed.idem_key);
    let prefix = payload.len() - (4 + content.len()) - tail;
    let mut out = Vec::with_capacity(prefix + 8 + stream.len() + tail);
    out.push(DEFLATE);
    out.extend_from_slice(&payload[1..prefix]);
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    bytes(&mut out, &stream);
    out.extend_from_slice(&payload[payload.len() - tail..]);
    debug_assert_eq!(out.capacity(), out.len());
    Ok(out)
}

/// The version 1 bytes of a payload: itself for version 1, re-encoded for version 2. Retry
/// digests cover these, so a retry matches its original whether either side was compressed.
pub fn canonical(payload: &[u8]) -> Result<Cow<'_, [u8]>, CodecError> {
    let parsed = decode_ref(payload)?;
    if let Content::Plain(_) = parsed.content {
        return Ok(Cow::Borrowed(payload));
    }
    let turn = NewTurn {
        role: parsed.role,
        content: parsed.content.bytes()?.into_owned(),
        meta: parsed.meta.to_vec(),
    };
    let (agent, session) = (parsed.agent_id, parsed.session_id);
    encode(agent, session, parsed.seq, &turn, parsed.idem_key).map(Cow::Owned)
}

/// Bytes after the content field: the retry key's presence byte and its blob.
fn tail_len(idem_key: Option<&str>) -> usize {
    1 + idem_key.map_or(0, |key| 4 + key.len())
}

/// Turn content as stored. Replay never inflates it.
pub(crate) enum Content<'a> {
    Plain(&'a [u8]),
    Deflate { raw_len: usize, stream: &'a [u8] },
}

impl<'a> Content<'a> {
    /// The content bytes. Inflating stops at `raw_len` and must produce exactly that many.
    pub(crate) fn bytes(&self) -> Result<Cow<'a, [u8]>, CodecError> {
        match *self {
            Self::Plain(content) => Ok(Cow::Borrowed(content)),
            Self::Deflate { raw_len, stream } => {
                let out = miniz_oxide::inflate::decompress_to_vec_with_limit(stream, raw_len)
                    .map_err(|_| CodecError)?;
                if out.len() != raw_len {
                    return Err(CodecError);
                }
                Ok(Cow::Owned(out))
            }
        }
    }
}

pub(crate) struct ParsedTurn<'a> {
    pub(crate) agent_id: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) seq: u64,
    role: Role,
    content: Content<'a>,
    meta: &'a [u8],
    pub(crate) idem_key: Option<&'a str>,
}

impl ParsedTurn<'_> {
    /// The version 1 size of a payload whose stored size is `stored`, without inflating.
    pub(crate) fn logical_len(&self, stored: usize) -> usize {
        match self.content {
            Content::Plain(_) => stored,
            Content::Deflate { raw_len, stream } => stored - 4 - stream.len() + raw_len,
        }
    }
}
pub(crate) fn decode_ref(payload: &[u8]) -> Result<ParsedTurn<'_>, CodecError> {
    let mut cur = Cursor(payload);
    let version = cur.byte()?;
    if version != PLAIN && version != DEFLATE {
        return Err(CodecError);
    }
    let agent_id = std::str::from_utf8(cur.blob(255)?).map_err(|_| CodecError)?;
    let session_id = std::str::from_utf8(cur.blob(255)?).map_err(|_| CodecError)?;
    let seq = cur.u64()?;
    let role = match cur.byte()? {
        0 => Role::System,
        1 => Role::User,
        2 => Role::Assistant,
        3 => Role::Tool,
        _ => return Err(CodecError),
    };
    let meta = cur.blob(65536)?;
    let content = if version == PLAIN {
        Content::Plain(cur.blob(MAX_CONTENT)?)
    } else {
        let raw_len = cur.u32()? as usize;
        if raw_len == 0 || raw_len > MAX_CONTENT {
            return Err(CodecError);
        }
        Content::Deflate { raw_len, stream: cur.blob(MAX_CONTENT)? }
    };
    let idem_key = match cur.byte()? {
        0 => None,
        1 => Some(std::str::from_utf8(cur.blob(128)?).map_err(|_| CodecError)?),
        _ => return Err(CodecError),
    };
    cur.end()?;
    Ok(ParsedTurn { agent_id, session_id, seq, role, content, meta, idem_key })
}

pub(crate) fn set_seq(payload: &mut [u8], seq: u64) -> Result<(), CodecError> {
    let mut cur = Cursor(payload);
    cur.take(1)?;
    cur.blob(255)?;
    cur.blob(255)?;
    let offset = payload.len() - cur.0.len();
    payload.get_mut(offset..offset + 8).ok_or(CodecError)?.copy_from_slice(&seq.to_le_bytes());
    Ok(())
}
