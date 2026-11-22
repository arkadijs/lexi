//! Client attestation for the service's `shared-key` mode (SPEC §5.4):
//! `X-Lexi-Attest: <unix-ts>.<hex(hmac_sha256(key, ts))>`. Weak by design (the key ships in the
//! app), an MVP measure until Play Integrity (Phase 7). Computed fresh for every token request so
//! reconnects never reuse a stale timestamp.

use super::Secret;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Header value for `unix_secs`, or `None` if the key is unusable (never happens for HMAC).
pub fn header(key: &Secret, unix_secs: u64) -> Option<String> {
    let ts = unix_secs.to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(key.expose().as_bytes()).ok()?;
    mac.update(ts.as_bytes());
    let tag = mac.finalize().into_bytes();
    let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("{ts}.{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_independent_hmac() {
        // printf 1000 | openssl dgst -sha256 -hmac k
        assert_eq!(
            header(&Secret::new("k"), 1000).as_deref(),
            Some("1000.1f18631dc55a63d09fc820c0bad4d8b2e815e14cc56e321a2f92e963aca99ba1")
        );
    }
}
