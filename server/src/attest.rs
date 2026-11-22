//! Client attestation (SPEC §5.4).

use crate::config::{AttestMode, Config, Secret};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

/// Allowed clock skew for the `shared-key` timestamp, in seconds.
const MAX_SKEW_SECS: u64 = 300;

#[derive(Debug, PartialEq, Eq)]
pub struct AttestError;

#[derive(Debug)]
pub enum Attestor {
    Off,
    /// `X-Lexi-Attest: <unix-ts>.<hex(hmac_sha256(key, ts))>`. Weak (key is extractable), MVP only.
    SharedKey(Secret),
    #[cfg(feature = "play-integrity")]
    PlayIntegrity,
}

impl Attestor {
    pub fn from_config(cfg: &Config) -> Result<Self, String> {
        match cfg.attest_mode {
            AttestMode::Off => Ok(Self::Off),
            AttestMode::SharedKey => cfg
                .attest_shared_key
                .clone()
                .map(Self::SharedKey)
                .ok_or_else(|| "shared key missing".to_owned()),
            #[cfg(feature = "play-integrity")]
            AttestMode::PlayIntegrity => Ok(Self::PlayIntegrity),
            #[cfg(not(feature = "play-integrity"))]
            AttestMode::PlayIntegrity => Err("play-integrity feature not compiled in".into()),
        }
    }

    pub fn verify(&self, header: Option<&str>) -> Result<(), AttestError> {
        match self {
            Self::Off => Ok(()),
            Self::SharedKey(key) => verify_shared(key, header.ok_or(AttestError)?, now_secs()),
            // Stub: real verification via Google's API lands in Phase 7. Fail closed.
            #[cfg(feature = "play-integrity")]
            Self::PlayIntegrity => Err(AttestError),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mac(key: &Secret, ts: &str) -> Result<Hmac<Sha256>, AttestError> {
    let mut m = Hmac::<Sha256>::new_from_slice(key.expose().as_bytes()).map_err(|_| AttestError)?;
    m.update(ts.as_bytes());
    Ok(m)
}

fn verify_shared(key: &Secret, header: &str, now: u64) -> Result<(), AttestError> {
    let (ts, sig_hex) = header.split_once('.').ok_or(AttestError)?;
    let ts_val: u64 = ts.parse().map_err(|_| AttestError)?;
    if now.abs_diff(ts_val) > MAX_SKEW_SECS {
        return Err(AttestError);
    }
    let sig = decode_hex(sig_hex).ok_or(AttestError)?;
    // `verify_slice` compares in constant time.
    mac(key, ts)?.verify_slice(&sig).map_err(|_| AttestError)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Build a valid header value (used by tests and documentation of the client side).
pub fn sign(key: &Secret, ts: u64) -> Result<String, AttestError> {
    let ts = ts.to_string();
    let tag = mac(key, &ts)?.finalize().into_bytes();
    let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{ts}.{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_fresh_signature() {
        let k = Secret::new("k");
        let h = sign(&k, 1_000).unwrap();
        assert_eq!(verify_shared(&k, &h, 1_100), Ok(()));
    }

    #[test]
    fn rejects_stale_wrong_key_and_garbage() {
        let k = Secret::new("k");
        let h = sign(&k, 1_000).unwrap();
        assert!(verify_shared(&k, &h, 1_000 + MAX_SKEW_SECS + 1).is_err());
        assert!(verify_shared(&Secret::new("other"), &h, 1_000).is_err());
        assert!(verify_shared(&k, "nodot", 1_000).is_err());
        assert!(verify_shared(&k, "1000.zz", 1_000).is_err());
        assert!(verify_shared(&k, "abc.00", 1_000).is_err());
    }
}
