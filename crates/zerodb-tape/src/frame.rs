//! Checksummed frames and canonical per-tenant hash chains.

use crate::crc32c::crc32c;
use std::fmt;

/// The fixed header size, including CRC and total length.
pub const HEADER_LEN: usize = 104;
/// The maximum encoded frame size, including its header.
pub const MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;
/// The supported frame version.
pub const VERSION: u8 = 1;
/// A husk retains its header and digest without any payload.
pub const FLAG_HUSK: u16 = 1;
/// A BLAKE3 digest or chain head.
pub type Digest = [u8; 32];

/// A tape record kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    /// A session turn.
    Turn = 1,
    /// A fact revision.
    Fact = 2,
    /// An unchained cache entry.
    Cache = 3,
    /// A chained redaction record.
    Redaction = 4,
    /// An unchained transaction footer.
    Footer = 5,
}

impl TryFrom<u8> for Kind {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Turn),
            2 => Ok(Self::Fact),
            3 => Ok(Self::Cache),
            4 => Ok(Self::Redaction),
            5 => Ok(Self::Footer),
            _ => Err(DecodeError::BadLength),
        }
    }
}

/// Frame metadata. CRC and total length are derived during encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    /// The record kind.
    pub kind: Kind,
    /// The frame format version.
    pub version: u8,
    /// Storage flags. Bit 0 marks a husk.
    pub flags: u16,
    /// The transaction identifier.
    pub txn: u64,
    /// The log sequence number.
    pub lsn: u64,
    /// The commit timestamp in nanoseconds since the Unix epoch.
    pub commit_time_ns: u64,
    /// The tenant identifier.
    pub tenant_id: u32,
    /// The previous tenant chain head. Cache and Footer require zeros.
    pub tenant_prev_chain: Digest,
    /// The salted content digest, retained by husks.
    pub content_digest: Digest,
}

/// A decoded frame and its payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Frame metadata.
    pub header: Header,
    /// Salt followed by content, footer fields, or nothing for a husk.
    pub payload: Vec<u8>,
}

/// A transaction footer. Its payload is exactly 44 bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Footer {
    /// The transaction identifier.
    pub txn: u64,
    /// The number of records before this footer.
    pub record_count: u32,
    /// The ordered batch content digest.
    pub batch_digest: Digest,
}

impl Footer {
    /// Encodes footer fields in little-endian order.
    pub fn encode(&self) -> [u8; 44] {
        let mut payload = [0; 44];
        payload[..8].copy_from_slice(&self.txn.to_le_bytes());
        payload[8..12].copy_from_slice(&self.record_count.to_le_bytes());
        payload[12..].copy_from_slice(&self.batch_digest);
        payload
    }

    /// Decodes an exact footer payload.
    pub fn decode(payload: &[u8]) -> Result<Self, DecodeError> {
        if payload.len() != 44 {
            return Err(DecodeError::BadLength);
        }
        Ok(Self {
            txn: u64::from_le_bytes(field(payload, 0)?),
            record_count: u32::from_le_bytes(field(payload, 8)?),
            batch_digest: field(payload, 12)?,
        })
    }
}

/// A frame cannot be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// A zero-length, zero-CRC end marker was reached.
    EndOfData,
    /// The fixed header is incomplete.
    Truncated,
    /// The stored CRC does not match the frame bytes.
    BadCrc,
    /// The length or frame structure is invalid.
    BadLength,
    /// The checksummed frame uses an unsupported version.
    UnknownVersion,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EndOfData => "end of written data",
            Self::Truncated => "truncated frame header",
            Self::BadCrc => "frame CRC mismatch",
            Self::BadLength => "invalid frame length or structure",
            Self::UnknownVersion => "unsupported frame version",
        })
    }
}

impl std::error::Error for DecodeError {}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], DecodeError> {
    bytes
        .get(offset..offset + N)
        .ok_or(DecodeError::Truncated)?
        .try_into()
        .map_err(|_| DecodeError::Truncated)
}

fn validate(header: &Header, payload: &[u8]) -> Result<(), DecodeError> {
    if header.version != VERSION {
        return Err(DecodeError::UnknownVersion);
    }
    if header.flags & !FLAG_HUSK != 0
        || (matches!(header.kind, Kind::Cache | Kind::Footer)
            && header.tenant_prev_chain != [0; 32])
    {
        return Err(DecodeError::BadLength);
    }
    if header.flags & FLAG_HUSK != 0 {
        if !payload.is_empty() {
            return Err(DecodeError::BadLength);
        }
    } else if (header.kind == Kind::Footer && payload.len() != 44)
        || (header.kind != Kind::Footer && payload.len() < 16)
    {
        return Err(DecodeError::BadLength);
    }
    Ok(())
}

/// Encodes a frame with a CRC over the length, header, and payload.
///
/// Returns `BatchTooLarge` above 4 MiB, or `InvalidFrame` for invalid metadata or payload structure.
pub fn encode(frame: &Frame) -> crate::Result<Vec<u8>> {
    if frame.payload.len() > MAX_FRAME_SIZE - HEADER_LEN {
        return Err(crate::Error::BatchTooLarge);
    }
    validate(&frame.header, &frame.payload)
        .map_err(|error| crate::Error::InvalidFrame(error.to_string()))?;
    let total = HEADER_LEN + frame.payload.len();
    let header = &frame.header;
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(total as u32).to_le_bytes());
    bytes.push(header.kind as u8);
    bytes.push(header.version);
    bytes.extend_from_slice(&header.flags.to_le_bytes());
    bytes.extend_from_slice(&header.txn.to_le_bytes());
    bytes.extend_from_slice(&header.lsn.to_le_bytes());
    bytes.extend_from_slice(&header.commit_time_ns.to_le_bytes());
    bytes.extend_from_slice(&header.tenant_id.to_le_bytes());
    bytes.extend_from_slice(&header.tenant_prev_chain);
    bytes.extend_from_slice(&header.content_digest);
    bytes.extend_from_slice(&frame.payload);
    let crc = crc32c(&bytes[4..]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

/// Decodes one frame and returns its consumed byte count.
///
/// A partial fixed header is `Truncated`. A length beyond the supplied bytes is `BadLength`.
/// End markers require zero CRC bytes, so a damaged length cannot hide a frame.
pub fn decode(bytes: &[u8]) -> Result<(Frame, usize), DecodeError> {
    let crc = u32::from_le_bytes(field(bytes, 0)?);
    let total = u32::from_le_bytes(field(bytes, 4)?) as usize;
    if total == 0 {
        return Err(if crc == 0 { DecodeError::EndOfData } else { DecodeError::BadLength });
    }
    if bytes.len() < HEADER_LEN {
        return Err(DecodeError::Truncated);
    }
    if !(HEADER_LEN..=MAX_FRAME_SIZE).contains(&total) || total > bytes.len() {
        return Err(DecodeError::BadLength);
    }
    if crc32c(&bytes[4..total]) != crc {
        return Err(DecodeError::BadCrc);
    }
    if bytes[9] != VERSION {
        return Err(DecodeError::UnknownVersion);
    }
    let header = Header {
        kind: Kind::try_from(bytes[8])?,
        version: bytes[9],
        flags: u16::from_le_bytes(field(bytes, 10)?),
        txn: u64::from_le_bytes(field(bytes, 12)?),
        lsn: u64::from_le_bytes(field(bytes, 20)?),
        commit_time_ns: u64::from_le_bytes(field(bytes, 28)?),
        tenant_id: u32::from_le_bytes(field(bytes, 36)?),
        tenant_prev_chain: field(bytes, 40)?,
        content_digest: field(bytes, 72)?,
    };
    let payload = &bytes[HEADER_LEN..total];
    validate(&header, payload)?;
    Ok((Frame { header, payload: payload.to_vec() }, total))
}

/// Encodes only canonical chain fields, in exactly 30 little-endian bytes.
pub fn canon(header: &Header) -> [u8; 30] {
    let mut bytes = [0; 30];
    bytes[0] = header.kind as u8;
    bytes[1] = header.version;
    bytes[2..10].copy_from_slice(&header.txn.to_le_bytes());
    bytes[10..18].copy_from_slice(&header.lsn.to_le_bytes());
    bytes[18..26].copy_from_slice(&header.commit_time_ns.to_le_bytes());
    bytes[26..30].copy_from_slice(&header.tenant_id.to_le_bytes());
    bytes
}

/// Hashes a 16-byte salt followed by content bytes.
pub fn content_digest(salt: &[u8; 16], content: &[u8]) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(salt);
    hasher.update(content);
    *hasher.finalize().as_bytes()
}

/// Hashes the previous tenant head, canonical header, and retained content digest.
pub fn chain_next(prev: &Digest, header: &Header, digest: &Digest) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(prev);
    hasher.update(&canon(header));
    hasher.update(digest);
    *hasher.finalize().as_bytes()
}

/// Hashes ordered content digests without including raw payload bytes.
pub fn batch_digest(digests: &[Digest]) -> Digest {
    let mut hasher = blake3::Hasher::new();
    for digest in digests {
        hasher.update(digest);
    }
    *hasher.finalize().as_bytes()
}
