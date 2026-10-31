//! Bearer token authentication. The tenant comes only from the matching token.

use crate::config::{Tenant, Token};
use crate::http::{Head, Response};
use serde_json::Value;
use std::fmt;

/// The request header that carries a client's nonce on `GET /healthz`.
pub const NONCE_HEADER: &str = "X-Atticdb-Nonce";
/// Nonce length in bytes. It travels as twice as many hex digits.
pub const NONCE_BYTES: usize = 32;
/// The `derive_key` context of a token's proof key.
const PROOF_CONTEXT: &str = "atticdb healthz proof v1";

/// Token hashes for every configured tenant.
pub struct Auth {
    tenants: Vec<(String, blake3::Hash)>,
    /// Each tenant's [`proof_key`], in config order.
    proof_keys: Vec<[u8; 32]>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth").field("tenants", &self.tenants.len()).finish()
    }
}

impl Auth {
    /// Hashes each tenant's token once, and derives its proof key.
    pub fn new(tenants: &[Tenant]) -> Self {
        let proof_keys = tenants.iter().map(|tenant| proof_key(&tenant.token)).collect();
        let tenants = tenants
            .iter()
            .map(|tenant| (tenant.name.clone(), blake3::hash(tenant.token.expose().as_bytes())))
            .collect();
        Self { tenants, proof_keys }
    }

    /// The `/healthz` proofs for the request's nonce, one per tenant in config order, as hex.
    /// `None` without exactly 1 nonce header of [`NONCE_BYTES`] in hex.
    pub fn proofs(&self, head: &Head, instance: &str) -> Option<Vec<String>> {
        let mut values = head.headers("x-atticdb-nonce");
        let (Some(value), None) = (values.next(), values.next()) else {
            return None;
        };
        let nonce = parse_nonce(value)?;
        let proofs = self.proof_keys.iter().map(|key| proof(key, &nonce, instance).to_hex());
        Some(proofs.map(|hex| hex.to_string()).collect())
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

/// The key a server proves it knows `token` with: `derive_key` over the token's bytes.
pub fn proof_key(token: &Token) -> [u8; 32] {
    blake3::derive_key(PROOF_CONTEXT, token.expose().as_bytes())
}

/// `keyed_hash(key, nonce || instance)`, where `instance` is the id's text as in `LOCK`.
pub fn proof(key: &[u8; 32], nonce: &[u8; NONCE_BYTES], instance: &str) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(nonce);
    hasher.update(instance.as_bytes());
    hasher.finalize()
}

/// Exactly [`NONCE_BYTES`] as hex digits, in either case.
pub fn parse_nonce(text: &[u8]) -> Option<[u8; NONCE_BYTES]> {
    if text.len() != 2 * NONCE_BYTES {
        return None;
    }
    let digit = |byte: u8| char::from(byte).to_digit(16);
    let mut nonce = [0; NONCE_BYTES];
    for (index, byte) in nonce.iter_mut().enumerate() {
        let value = digit(text[2 * index])? << 4 | digit(text[2 * index + 1])?;
        *byte = u8::try_from(value).ok()?;
    }
    Some(nonce)
}

/// Whether `proofs`, the `/healthz` array, holds the proof for `token`, `nonce` and
/// `instance`. Every entry is compared, each in constant time, without early exit.
pub fn verify(token: &Token, nonce: &[u8; NONCE_BYTES], instance: &str, proofs: &Value) -> bool {
    let expected = proof(&proof_key(token), nonce, instance);
    let mut found = false;
    for entry in proofs.as_array().map_or(&[][..], Vec::as_slice) {
        let hash = entry.as_str().and_then(|hex| blake3::Hash::from_hex(hex).ok());
        // `blake3::Hash` equality is constant time.
        found |= hash.is_some_and(|hash| hash == expected);
    }
    found
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{parse_nonce, proof, proof_key, verify, Auth, NONCE_BYTES};
    use crate::config::{Tenant, Token};
    use serde_json::json;

    fn token(text: &str) -> Token {
        Token::new(text.repeat(32)).unwrap()
    }

    #[test]
    fn nonces_are_exactly_64_hex_digits() {
        let hex = "0123456789abcdefABCDEF".repeat(3);
        let nonce = parse_nonce(&hex.as_bytes()[..64]).unwrap();
        assert_eq!(nonce[..3], [0x01, 0x23, 0x45]);
        assert_eq!(parse_nonce(&[b'a'; 63]), None);
        assert_eq!(parse_nonce(&[b'a'; 65]), None);
        // A sign is not a digit.
        let signed = format!("+f{}", "0".repeat(62));
        assert_eq!(parse_nonce(signed.as_bytes()), None);
        assert_eq!(parse_nonce(&[b'g'; 64]), None);
    }

    #[test]
    fn the_proof_follows_the_derivation() {
        let (secret, nonce) = (token("a"), [7; NONCE_BYTES]);
        let key = blake3::derive_key("atticdb healthz proof v1", secret.expose().as_bytes());
        assert_eq!(proof_key(&secret), key);
        let mut message = nonce.to_vec();
        message.extend_from_slice(b"0123");
        assert_eq!(proof(&key, &nonce, "0123"), blake3::keyed_hash(&key, &message));
    }

    #[test]
    fn only_the_right_token_nonce_and_instance_verify() {
        let (a, b) = (token("a"), token("b"));
        let tenants = [
            Tenant { name: "a".into(), token: a.clone() },
            Tenant { name: "b".into(), token: b.clone() },
        ];
        let auth = Auth::new(&tenants);
        let nonce = [9; NONCE_BYTES];
        let hex = "09".repeat(NONCE_BYTES);
        let head = crate::http::Head::for_test("GET", "/healthz", &[("x-atticdb-nonce", &hex)]);
        let proofs = json!(auth.proofs(&head, "0123").unwrap());
        assert_eq!(proofs.as_array().unwrap().len(), 2);
        assert!(verify(&a, &nonce, "0123", &proofs));
        assert!(verify(&b, &nonce, "0123", &proofs));
        assert!(!verify(&token("c"), &nonce, "0123", &proofs), "another token");
        assert!(!verify(&a, &[8; NONCE_BYTES], "0123", &proofs), "another nonce");
        assert!(!verify(&a, &nonce, "0124", &proofs), "another instance");
        for bad in [json!(null), json!([]), json!(["zz"]), json!([1]), json!("x")] {
            assert!(!verify(&a, &nonce, "0123", &bad), "{bad}");
        }
        let none = crate::http::Head::for_test("GET", "/healthz", &[]);
        assert_eq!(auth.proofs(&none, "0123"), None);
        let twice = [("x-atticdb-nonce", hex.as_str()), ("x-atticdb-nonce", hex.as_str())];
        let twice = crate::http::Head::for_test("GET", "/healthz", &twice);
        assert_eq!(auth.proofs(&twice, "0123"), None);
        let short = crate::http::Head::for_test("GET", "/healthz", &[("x-atticdb-nonce", "09")]);
        assert_eq!(auth.proofs(&short, "0123"), None);
    }
}
