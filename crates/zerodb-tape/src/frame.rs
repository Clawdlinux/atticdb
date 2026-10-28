//! Location-bound CRC32C frames.

use crate::crc32c::Crc32c;
use std::fmt;

/// Fixed header size, including CRC and total length.
pub const HEADER_LEN: usize = 40;
/// Maximum encoded frame size.
pub const MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;
/// Supported frame version.
pub const VERSION: u8 = 2;
/// An application or checkpoint digest.
pub type Digest = [u8; 32];

/// A tape record kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Kind {
    /// A session turn.
    Turn = 1,
    /// A fact revision.
    Fact = 2,
    /// A request to hide tenant-local content.
    Forget = 3,
    /// A transaction footer.
    Footer = 5,
    /// A tenant registry entry.
    TenantCreate = 6,
}

impl TryFrom<u8> for Kind {
    type Error = DecodeError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Turn),
            2 => Ok(Self::Fact),
            3 => Ok(Self::Forget),
            5 => Ok(Self::Footer),
            6 => Ok(Self::TenantCreate),
            _ => Err(DecodeError::BadLength),
        }
    }
}

/// Frame metadata. Encoding derives CRC and length.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    /// Record kind.
    pub kind: Kind,
    /// Format version.
    pub version: u8,
    /// Reserved. Must be zero.
    pub flags: u16,
    /// Transaction identifier.
    pub txn: u64,
    /// Record LSN. Footers repeat the final record LSN.
    pub lsn: u64,
    /// Commit timestamp in nanoseconds since the Unix epoch.
    pub commit_time_ns: u64,
    /// Tenant identifier.
    pub tenant_id: u32,
}

/// A frame and its payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Frame metadata.
    pub header: Header,
    /// Application bytes or footer fields.
    pub payload: Vec<u8>,
}

/// A footer bound to exactly the preceding records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Footer {
    /// Transaction identifier.
    pub txn: u64,
    /// First record LSN.
    pub first_lsn: u64,
    /// LSN actually synced before building this footer.
    pub synced_lsn: u64,
    /// Number of records.
    pub record_count: u32,
    /// CRC32C of the concatenated little-endian record CRCs.
    pub records_crc: u32,
}

impl Footer {
    /// Encodes exactly 32 bytes in little-endian order.
    pub fn encode(&self) -> [u8; 32] {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&self.txn.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.first_lsn.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.synced_lsn.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.record_count.to_le_bytes());
        bytes[28..].copy_from_slice(&self.records_crc.to_le_bytes());
        bytes
    }

    /// Decodes an exact footer payload.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != 32 {
            return Err(DecodeError::BadLength);
        }
        Ok(Self {
            txn: u64::from_le_bytes(field(bytes, 0)?),
            first_lsn: u64::from_le_bytes(field(bytes, 8)?),
            synced_lsn: u64::from_le_bytes(field(bytes, 16)?),
            record_count: u32::from_le_bytes(field(bytes, 24)?),
            records_crc: u32::from_le_bytes(field(bytes, 28)?),
        })
    }
}

/// A frame cannot be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// An empty tail or zero-CRC, zero-length marker.
    EndOfData,
    /// The fixed header is incomplete.
    Truncated,
    /// The stored CRC does not match the bytes and location.
    BadCrc,
    /// The length or structure is invalid.
    BadLength,
    /// The frame version is unsupported.
    UnknownVersion,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EndOfData => "end of written data",
            Self::Truncated => "truncated frame header",
            Self::BadCrc => "frame CRC mismatch",
            Self::BadLength => "invalid frame length or structure",
            Self::UnknownVersion => "unsupported frame version: expected v2",
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
    if header.flags != 0 || (header.kind == Kind::Footer && payload.len() != 32) {
        return Err(DecodeError::BadLength);
    }
    Ok(())
}

/// CRC32C over the location followed by frame bytes starting at the length field.
pub fn checksum(segment: u64, offset: u64, bytes: &[u8]) -> u32 {
    let mut seed = [0; 16];
    seed[..8].copy_from_slice(&segment.to_le_bytes());
    seed[8..].copy_from_slice(&offset.to_le_bytes());
    let mut crc = Crc32c::new();
    crc.update(&seed);
    crc.update(bytes);
    crc.finish()
}

/// Encodes a frame bound to a manifest segment id and byte offset.
pub fn encode(frame: &Frame, segment: u64, offset: u64) -> crate::Result<Vec<u8>> {
    if frame.payload.len() > MAX_FRAME_SIZE - HEADER_LEN {
        return Err(crate::Error::BatchTooLarge);
    }
    validate(&frame.header, &frame.payload)
        .map_err(|e| crate::Error::InvalidFrame(e.to_string()))?;
    let total = HEADER_LEN + frame.payload.len();
    let h = &frame.header;
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(total as u32).to_le_bytes());
    bytes.extend_from_slice(&[h.kind as u8, h.version]);
    bytes.extend_from_slice(&h.flags.to_le_bytes());
    bytes.extend_from_slice(&h.txn.to_le_bytes());
    bytes.extend_from_slice(&h.lsn.to_le_bytes());
    bytes.extend_from_slice(&h.commit_time_ns.to_le_bytes());
    bytes.extend_from_slice(&h.tenant_id.to_le_bytes());
    bytes.extend_from_slice(&frame.payload);
    let crc = checksum(segment, offset, &bytes[4..]);
    bytes[..4].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

/// Parses the header fields without the CRC, which covers the payload too. Only [`decode`]
/// proves a frame intact.
pub fn parse_header(bytes: &[u8; HEADER_LEN]) -> Result<Header, DecodeError> {
    Ok(Header {
        kind: Kind::try_from(bytes[8])?,
        version: bytes[9],
        flags: u16::from_le_bytes(field(bytes, 10)?),
        txn: u64::from_le_bytes(field(bytes, 12)?),
        lsn: u64::from_le_bytes(field(bytes, 20)?),
        commit_time_ns: u64::from_le_bytes(field(bytes, 28)?),
        tenant_id: u32::from_le_bytes(field(bytes, 36)?),
    })
}

/// Decodes one frame at its manifest location.
pub fn decode(bytes: &[u8], segment: u64, offset: u64) -> Result<(Frame, usize), DecodeError> {
    if bytes.is_empty() {
        return Err(DecodeError::EndOfData);
    }
    let crc = u32::from_le_bytes(field(bytes, 0)?);
    let total = u32::from_le_bytes(field(bytes, 4)?) as usize;
    if total == 0 {
        return Err(if crc == 0 { DecodeError::EndOfData } else { DecodeError::BadLength });
    }
    let Some(head) = bytes.first_chunk::<HEADER_LEN>() else {
        return Err(DecodeError::Truncated);
    };
    if !(HEADER_LEN..=MAX_FRAME_SIZE).contains(&total) || total > bytes.len() {
        return Err(DecodeError::BadLength);
    }
    if bytes[9] == 1 && crate::crc32c::crc32c(&bytes[4..total]) == crc {
        return Err(DecodeError::UnknownVersion);
    }
    if checksum(segment, offset, &bytes[4..total]) != crc {
        return Err(DecodeError::BadCrc);
    }
    let header = parse_header(head)?;
    let payload = &bytes[HEADER_LEN..total];
    validate(&header, payload)?;
    Ok((Frame { header, payload: payload.to_vec() }, total))
}
