//! Bearer token authentication. The tenant comes only from the matching token.

use crate::config::Tenant;
use crate::http::{Head, Response};
use std::fmt;

/// Token hashes for every configured tenant.
pub struct Auth {
    tenants: Vec<(String, blake3::Hash)>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth").field("tenants", &self.tenants.len()).finish()
    }
}

impl Auth {
    /// Hashes each tenant's token once.
    pub fn new(tenants: &[Tenant]) -> Self {
        let tenants = tenants
            .iter()
            .map(|tenant| (tenant.name.clone(), blake3::hash(tenant.token.expose().as_bytes())))
            .collect();
        Self { tenants }
    }

    /// Returns the tenant whose token the request presents, or `None` for a missing,
    /// repeated, malformed or unknown `Authorization` header. The presented token is
    /// hashed and compared with every tenant's hash in constant time, without early exit.
    pub fn tenant(&self, head: &Head) -> Option<&str> {
        let mut values = head.headers("authorization");
        let (Some(value), None) = (values.next(), values.next()) else {
            return None;
        };
        let presented = blake3::hash(bearer(value)?);
        let mut found = None;
        for (name, hash) in &self.tenants {
            if *hash == presented {
                found = Some(name.as_str());
            }
        }
        found
    }
}

/// The 401 response, the same for a missing and a bad token.
pub fn unauthorized() -> Response {
    Response::error(401, "unauthorized", None)
}

/// `Bearer <token>`, scheme case-insensitive.
fn bearer(value: &[u8]) -> Option<&[u8]> {
    let space = value.iter().position(|byte| *byte == b' ')?;
    let (scheme, token) = value.split_at(space);
    let token = token.trim_ascii();
    (scheme.eq_ignore_ascii_case(b"Bearer") && !token.is_empty()).then_some(token)
}
