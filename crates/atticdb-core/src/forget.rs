//! Version 1 forget payloads. Selectors never contain a tenant.

use crate::fact::{get_key, lsns, put_key};
use crate::turn::{bytes, CodecError, Cursor};

/// A selector resolved inside the caller's tenant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selector {
    /// A session identified by agent and session, each at most 255 UTF-8 bytes.
    Session {
        /// Agent identifier.
        agent: String,
        /// Session identifier.
        session: String,
    },
    /// A fact key, at most 1024 UTF-8 bytes.
    FactKey(String),
    /// At most 1024 explicit LSNs.
    Lsns(Vec<u64>),
}

/// A hide request. The engine does not apply forgets yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Forget {
    /// Tenant-local target.
    pub selector: Selector,
    /// Optional retry key, at most 128 UTF-8 bytes.
    pub idem_key: Option<String>,
    /// Optional inclusive cutoff. Absence means this record's own LSN.
    pub up_to_lsn: Option<u64>,
}

/// Encodes an exact, length-limited forget payload.
pub fn encode(forget: &Forget) -> Result<Vec<u8>, CodecError> {
    if forget.idem_key.as_ref().is_some_and(|key| key.len() > 128) {
        return Err(CodecError);
    }
    let mut out = vec![1];
    match &forget.selector {
        Selector::Session { agent, session } if agent.len() <= 255 && session.len() <= 255 => {
            out.push(1);
            bytes(&mut out, agent.as_bytes());
            bytes(&mut out, session.as_bytes());
        }
        Selector::FactKey(key) if key.len() <= 1024 => {
            out.push(2);
            bytes(&mut out, key.as_bytes());
        }
        Selector::Lsns(lsns) if lsns.len() <= 1024 => {
            out.push(3);
            out.extend_from_slice(&(lsns.len() as u32).to_le_bytes());
            for lsn in lsns {
                out.extend_from_slice(&lsn.to_le_bytes());
            }
        }
        _ => return Err(CodecError),
    }
    put_key(&mut out, forget.idem_key.as_deref());
    out.push(u8::from(forget.up_to_lsn.is_some()));
    if let Some(lsn) = forget.up_to_lsn {
        out.extend_from_slice(&lsn.to_le_bytes());
    }
    Ok(out)
}

/// Decodes a forget, rejecting unknown versions, tags and trailing bytes.
pub fn decode(payload: &[u8]) -> Result<Forget, CodecError> {
    let mut cur = Cursor(payload);
    if cur.byte()? != 1 {
        return Err(CodecError);
    }
    let selector = match cur.byte()? {
        1 => Selector::Session { agent: cur.string(255)?, session: cur.string(255)? },
        2 => Selector::FactKey(cur.string(1024)?),
        3 => Selector::Lsns(lsns(&mut cur)?),
        _ => return Err(CodecError),
    };
    let idem_key = get_key(&mut cur)?;
    let up_to_lsn = match cur.byte()? {
        0 => None,
        1 => Some(cur.u64()?),
        _ => return Err(CodecError),
    };
    cur.end()?;
    Ok(Forget { selector, idem_key, up_to_lsn })
}
