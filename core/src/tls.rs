//! The one TLS crypto provider of the core: aws-lc-rs, which str0m's DTLS (dimpl) already links, so the
//! library carries a single crypto stack instead of two (`ring` as well cost ~250 KB, measured 2026-11-22).
//! Used by `ureq` (HTTPS to our server) and by the Gemini WebSocket.

use rustls::crypto::CryptoProvider;

pub fn provider() -> CryptoProvider {
    rustls::crypto::aws_lc_rs::default_provider()
}
