//! Configuration from environment variables (12-factor, SPEC §5.3).

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

/// Secret string whose `Debug` output never reveals the contents.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestMode {
    Off,
    SharedKey,
    PlayIntegrity,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub metrics_bind: SocketAddr,
    pub api_key: Option<Secret>,
    pub openai_base_url: String,
    pub model: String,
    /// Default voice, used when the app does not ask for one.
    pub voice: String,
    /// Voices the app may request (`voice` in the request body); anything else is a 400.
    pub voices: Vec<String>,
    pub instructions: String,
    /// `session.reasoning.effort`; `None` leaves the API default (`low`).
    pub reasoning_effort: Option<String>,
    /// server_vad `silence_duration_ms` (API default 500); `None` leaves the default.
    pub vad_silence_ms: Option<u32>,
    pub token_ttl_secs: u32,
    pub rate_per_install: u32,
    pub rate_per_ip: u32,
    pub attest_mode: AttestMode,
    pub attest_shared_key: Option<Secret>,
    /// Trust the rightmost `X-Forwarded-For` entry (set by our ingress) as the client IP.
    pub trust_forwarded_for: bool,
    pub upstream_timeout: Duration,
    pub request_timeout: Duration,
}

/// The whole prompt. Language behaviour only: greet in Russian, expect Russian, follow the user if
/// they switch. (The client sends `response.create` when the session opens so the greeting is spoken.)
pub const DEFAULT_INSTRUCTIONS: &str = "At the very start of the conversation, before the user \
says anything, greet them with one short, warm sentence in Russian. Expect the user to speak \
Russian and reply in Russian by default. If the user speaks another language, or asks you to \
switch, switch to that language and keep using it until they switch again. In every language \
speak naturally, with native pronunciation and intonation.";

/// The built-in Realtime voices (OpenAI docs, verified 2026-11-21).
pub const DEFAULT_VOICES: &str = "alloy,ash,ballad,coral,echo,sage,shimmer,verse,marin,cedar";

const EFFORTS: [&str; 5] = ["minimal", "low", "medium", "high", "xhigh"];

/// OpenAI documents a 10 s minimum and 7200 s maximum for `expires_after.seconds`.
const TTL_RANGE: std::ops::RangeInclusive<u32> = 10..=7200;

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        fn parse<T: std::str::FromStr>(
            get: &impl Fn(&str) -> Option<String>,
            key: &str,
            default: T,
        ) -> Result<T, String> {
            match get(key) {
                None => Ok(default),
                Some(v) => v
                    .trim()
                    .parse()
                    .map_err(|_| format!("{key}: invalid value")),
            }
        }

        let attest_mode = match get("LEXI_ATTEST_MODE").as_deref().unwrap_or("off") {
            "off" => AttestMode::Off,
            "shared-key" => AttestMode::SharedKey,
            "play-integrity" => {
                if cfg!(feature = "play-integrity") {
                    AttestMode::PlayIntegrity
                } else {
                    return Err(
                        "LEXI_ATTEST_MODE=play-integrity needs the `play-integrity` feature".into(),
                    );
                }
            }
            other => return Err(format!("LEXI_ATTEST_MODE: unknown mode {other:?}")),
        };
        let attest_shared_key = get("LEXI_ATTEST_SHARED_KEY")
            .filter(|s| !s.is_empty())
            .map(Secret::new);
        if attest_mode == AttestMode::SharedKey && attest_shared_key.is_none() {
            return Err("LEXI_ATTEST_MODE=shared-key requires LEXI_ATTEST_SHARED_KEY".into());
        }

        let instructions = match get("LEXI_INSTRUCTIONS_FILE") {
            Some(path) => std::fs::read_to_string(&path)
                .map_err(|e| format!("LEXI_INSTRUCTIONS_FILE ({path}): {e}"))?,
            None => DEFAULT_INSTRUCTIONS.to_owned(),
        };

        let token_ttl_secs: u32 = parse(&get, "LEXI_TOKEN_TTL_SECS", 60)?;
        if !TTL_RANGE.contains(&token_ttl_secs) {
            return Err(format!("LEXI_TOKEN_TTL_SECS must be within {TTL_RANGE:?}"));
        }

        let voices: Vec<String> = get("LEXI_VOICES")
            .unwrap_or_else(|| DEFAULT_VOICES.to_owned())
            .split(',')
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .collect();
        let voice = get("LEXI_VOICE").unwrap_or_else(|| "cedar".into());
        if !voices.contains(&voice) {
            return Err(format!("LEXI_VOICE {voice:?} is not in LEXI_VOICES"));
        }
        // `low` is the API default; the deployed value comes from the ConfigMap (LEXI_REASONING_EFFORT).
        // `minimal` was measured ~0.2 s faster to first audio (docs/SPEC.md §2.1) but is not the default.
        let reasoning_effort = match get("LEXI_REASONING_EFFORT").as_deref().map(str::trim) {
            None => Some("low".to_owned()),
            Some("" | "default") => None,
            Some(e) if EFFORTS.contains(&e) => Some(e.to_owned()),
            Some(e) => return Err(format!("LEXI_REASONING_EFFORT: unknown effort {e:?}")),
        };
        let vad_silence_ms: u32 = parse(&get, "LEXI_VAD_SILENCE_MS", 400)?;
        if !(100..=5000).contains(&vad_silence_ms) && vad_silence_ms != 0 {
            return Err("LEXI_VAD_SILENCE_MS must be within 100..=5000 (0 = API default)".into());
        }

        Ok(Self {
            bind: parse(
                &get,
                "LEXI_BIND",
                "0.0.0.0:8080".parse().map_err(|_| "bind")?,
            )?,
            metrics_bind: parse(
                &get,
                "LEXI_METRICS_BIND",
                "0.0.0.0:9090".parse().map_err(|_| "metrics bind")?,
            )?,
            api_key: get("OPENAI_API_KEY")
                .filter(|s| !s.is_empty())
                .map(Secret::new),
            openai_base_url: get("LEXI_OPENAI_BASE_URL")
                .unwrap_or_else(|| "https://api.openai.com".into())
                .trim_end_matches('/')
                .to_owned(),
            model: get("LEXI_MODEL").unwrap_or_else(|| "gpt-realtime-2.1-mini".into()),
            voice,
            voices,
            instructions,
            reasoning_effort,
            vad_silence_ms: (vad_silence_ms != 0).then_some(vad_silence_ms),
            token_ttl_secs,
            rate_per_install: parse(&get, "LEXI_RATE_PER_INSTALL", 30)?,
            rate_per_ip: parse(&get, "LEXI_RATE_PER_IP", 120)?,
            attest_mode,
            attest_shared_key,
            trust_forwarded_for: parse(&get, "LEXI_TRUST_FORWARDED_FOR", false)?,
            upstream_timeout: Duration::from_millis(parse(&get, "LEXI_UPSTREAM_TIMEOUT_MS", 5000)?),
            request_timeout: Duration::from_millis(parse(&get, "LEXI_REQUEST_TIMEOUT_MS", 12000)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        Config::from_lookup(|k| owned.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()))
    }

    #[test]
    fn defaults() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.model, "gpt-realtime-2.1-mini");
        assert_eq!(c.token_ttl_secs, 60);
        assert!(c.api_key.is_none());
        assert_eq!(c.attest_mode, AttestMode::Off);
    }

    #[test]
    fn voice_and_latency_settings() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.voice, "cedar");
        assert!(c.voices.iter().any(|v| v == "cedar"));
        assert_eq!(c.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(c.vad_silence_ms, Some(400));
        let c = cfg(&[
            ("LEXI_REASONING_EFFORT", "default"),
            ("LEXI_VAD_SILENCE_MS", "0"),
        ])
        .unwrap();
        assert_eq!((c.reasoning_effort, c.vad_silence_ms), (None, None));
        assert!(cfg(&[("LEXI_VOICE", "nope")]).is_err());
        assert!(cfg(&[("LEXI_REASONING_EFFORT", "turbo")]).is_err());
        assert!(cfg(&[("LEXI_VAD_SILENCE_MS", "50")]).is_err());
        assert!(cfg(&[("LEXI_VOICES", "marin,cedar"), ("LEXI_VOICE", "coral")]).is_err());
    }

    #[test]
    fn rejects_bad_values() {
        assert!(cfg(&[("LEXI_TOKEN_TTL_SECS", "5")]).is_err());
        assert!(cfg(&[("LEXI_ATTEST_MODE", "shared-key")]).is_err());
        assert!(cfg(&[("LEXI_ATTEST_MODE", "bogus")]).is_err());
        assert!(cfg(&[("LEXI_RATE_PER_IP", "x")]).is_err());
    }

    #[test]
    fn secret_debug_is_redacted() {
        let c = cfg(&[("OPENAI_API_KEY", "sk-super-secret")]).unwrap();
        assert!(!format!("{c:?}").contains("sk-super-secret"));
    }
}
