//! Client attestation (SPEC §5.4).

use crate::config::{AttestMode, Config, Secret};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

/// Allowed clock skew for the `shared-key` timestamp, in seconds.
const MAX_SKEW_SECS: u64 = 300;

#[derive(Debug, PartialEq, Eq)]
pub enum AttestError {
    /// Missing, malformed or wrongly signed header.
    Invalid,
    /// The signature is valid for one of our keys but the timestamp is outside the allowed skew: the client's
    /// clock is wrong. Only reported after the signature checked out, so it tells an attacker nothing.
    Skew,
}

#[derive(Debug)]
pub enum Attestor {
    Off,
    /// `X-Lexi-Attest: <unix-ts>.<hex(hmac_sha256(key, ts))>` signed with any one of the accepted keys. Weak (a key
    /// is extractable from the APK), but enough for friends: keys are per friend and revocable.
    SharedKey(Vec<Secret>),
}

impl Attestor {
    pub fn from_config(cfg: &Config) -> Result<Self, String> {
        match cfg.attest_mode {
            AttestMode::Off => Ok(Self::Off),
            AttestMode::SharedKey if cfg.attest_keys.is_empty() => {
                Err("shared key missing".to_owned())
            }
            AttestMode::SharedKey => Ok(Self::SharedKey(cfg.attest_keys.clone())),
        }
    }

    pub fn verify(&self, header: Option<&str>) -> Result<(), AttestError> {
        match self {
            Self::Off => Ok(()),
            Self::SharedKey(keys) => {
                let (header, now) = (header.ok_or(AttestError::Invalid)?, now_secs());
                // Every key is checked (no early exit), so timing does not tell which one matched.
                let mut result = Err(AttestError::Invalid);
                for k in keys {
                    match verify_shared(k, header, now) {
                        Ok(()) => result = Ok(()),
                        Err(AttestError::Skew) if result.is_err() => {
                            result = Err(AttestError::Skew)
                        }
                        Err(_) => {}
                    }
                }
                result
            }
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
    let mut m = Hmac::<Sha256>::new_from_slice(key.expose().as_bytes())
        .map_err(|_| AttestError::Invalid)?;
    m.update(ts.as_bytes());
    Ok(m)
}

fn verify_shared(key: &Secret, header: &str, now: u64) -> Result<(), AttestError> {
    let (ts, sig_hex) = header.split_once('.').ok_or(AttestError::Invalid)?;
    let ts_val: u64 = ts.parse().map_err(|_| AttestError::Invalid)?;
    let sig = decode_hex(sig_hex).ok_or(AttestError::Invalid)?;
    // The signature first (`verify_slice` compares in constant time), the skew only for a valid one.
    mac(key, ts)?
        .verify_slice(&sig)
        .map_err(|_| AttestError::Invalid)?;
    if now.abs_diff(ts_val) > MAX_SKEW_SECS {
        return Err(AttestError::Skew);
    }
    Ok(())
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
    fn any_configured_key_is_accepted_and_nothing_else() {
        let cfg_keys = vec![Secret::new("alice"), Secret::new("bob")];
        let attestor = Attestor::SharedKey(cfg_keys);
        let now = now_secs();
        for who in ["alice", "bob"] {
            let h = sign(&Secret::new(who), now).unwrap();
            assert_eq!(attestor.verify(Some(&h)), Ok(()), "{who}");
        }
        let h = sign(&Secret::new("mallory"), now).unwrap();
        assert!(attestor.verify(Some(&h)).is_err());
        assert!(attestor.verify(None).is_err());
    }

    #[test]
    fn rejects_stale_wrong_key_and_garbage() {
        let k = Secret::new("k");
        let h = sign(&k, 1_000).unwrap();
        assert_eq!(
            verify_shared(&k, &h, 1_000 + MAX_SKEW_SECS + 1),
            Err(AttestError::Skew),
            "valid signature, wrong clock"
        );
        assert_eq!(
            verify_shared(&Secret::new("other"), &h, 1_000 + MAX_SKEW_SECS + 1),
            Err(AttestError::Invalid),
            "a wrong key never learns about the clock"
        );
        assert!(verify_shared(&Secret::new("other"), &h, 1_000).is_err());
        assert!(verify_shared(&k, "nodot", 1_000).is_err());
        assert!(verify_shared(&k, "1000.zz", 1_000).is_err());
        assert!(verify_shared(&k, "abc.00", 1_000).is_err());
    }
}
