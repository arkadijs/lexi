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
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub metrics_bind: SocketAddr,
    pub api_key: Option<Secret>,
    pub openai_base_url: String,
    pub model: String,
    /// Models the app may request (`model` in the request body); anything else is a 400.
    pub models: Vec<String>,
    /// Default voice, used when the app does not ask for one.
    pub voice: String,
    /// Google Gemini Live (second provider): key, model, base URL (tests), voices with genders, default voice,
    /// optional thinking level (`minimal`..`high`) and nothing else locked in unless set.
    pub google_api_key: Option<Secret>,
    pub gemini_base_url: String,
    pub gemini_model: String,
    pub gemini_voices: Vec<(String, Gender)>,
    pub gemini_voice: String,
    pub gemini_thinking: Option<String>,
    /// Gemini Live searches with Google's own `googleSearch` tool inside the session (`builtin`, default) or
    /// not at all (`off`). Providers are compartmentalized: OpenAI searches through OpenAI (`/v1/search`), Gemini
    /// through Google, and neither depends on the other's key.
    pub gemini_search: GeminiSearch,
    /// Voices the app may request (`voice` in the request body); anything else is a 400.
    pub voices: Vec<String>,
    pub instructions: String,
    /// Gender of each voice (`male`/`female`), so the bot uses the right grammatical gender about itself in
    /// languages that mark it (LEXI_VOICE_GENDERS, `voice=gender` pairs). Voices not listed get no gender line.
    pub voice_genders: Vec<(String, Gender)>,
    /// `session.reasoning.effort`; `None` leaves the API default (`low`).
    pub reasoning_effort: Option<String>,
    /// server_vad `silence_duration_ms` (API default 500); `None` leaves the default.
    pub vad_silence_ms: Option<u32>,
    /// Give the bot the `web_search` tool and serve `POST /v1/search` (LEXI_WEB_SEARCH, default on).
    pub web_search: bool,
    /// Responses-API model that runs the search (LEXI_SEARCH_MODEL).
    pub search_model: String,
    pub search_timeout: Duration,
    pub token_ttl_secs: u32,
    pub rate_per_install: u32,
    pub rate_per_ip: u32,
    /// Web searches per hour, per install and per IP (a conversation makes several).
    pub rate_search_per_install: u32,
    pub rate_search_per_ip: u32,
    pub attest_mode: AttestMode,
    /// Accepted `shared-key` keys (LEXI_ATTEST_SHARED_KEY, comma-separated).
    pub attest_keys: Vec<Secret>,
    /// Trust the rightmost `X-Forwarded-For` entry (set by our ingress) as the client IP.
    pub trust_forwarded_for: bool,
    pub upstream_timeout: Duration,
    pub request_timeout: Duration,
}

/// The built-in prompt: language behaviour (greet in Russian, expect Russian, follow a language switch) plus a
/// conversation style (an equal partner, hard length limits, no offers or closing questions, how to react to
/// "stop"). The client sends `response.create` when the session opens so the greeting is spoken. The same text
/// lives in `deploy/k8s/base/instructions.txt` (the ConfigMap that overrides it); a test keeps them identical.
pub const DEFAULT_INSTRUCTIONS: &str = include_str!("instructions.default.txt");

/// The built-in Realtime voices (OpenAI docs, verified 2026-11-21).
pub const DEFAULT_VOICES: &str = "alloy,ash,ballad,coral,echo,sage,shimmer,verse,marin,cedar";

/// The Realtime speech models the app may pick: the cheap default and the full one (prices in core/pricing).
pub const DEFAULT_MODELS: &str = "gpt-realtime-2.1-mini,gpt-realtime-2.1";

const EFFORTS: [&str; 5] = ["minimal", "low", "medium", "high", "xhigh"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiSearch {
    Builtin,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gender {
    Male,
    Female,
}

impl Gender {
    /// Appended to the instructions. Matters in Russian and other gendered languages ("рад" vs "рада").
    pub fn instruction(self) -> &'static str {
        match self {
            Gender::Male => "# Your gender\nYou are a man and your voice is male. In every language that marks grammatical gender (Russian, Ukrainian, Polish, German, French, Spanish and others) always speak about yourself in the masculine: in Russian \"рад\", \"я сказал\", \"готов\", never \"рада\", \"сказала\", \"готова\".",
            Gender::Female => "# Your gender\nYou are a woman and your voice is female. In every language that marks grammatical gender (Russian, Ukrainian, Polish, German, French, Spanish and others) always speak about yourself in the feminine: in Russian \"рада\", \"я сказала\", \"готова\", never \"рад\", \"сказал\", \"готов\".",
        }
    }
}

/// Default voice genders (the app offers Cedar, Marin and Alloy).
pub const DEFAULT_VOICE_GENDERS: &str = "cedar=male,marin=female,alloy=female";

/// Gemini voices offered by default with their genders (Google's voice list does not state them; these are
/// the commonly cited ones and are reviewed by ear). Names are lower-case here, capitalized for the API.
pub const DEFAULT_GEMINI_VOICES: &str = "charon=male,fenrir=male,kore=female,leda=female";

fn parse_voice_gender(p: &str) -> Result<(String, Gender), String> {
    match p.trim().split_once('=') {
        Some((v, "male")) => Ok((v.trim().to_owned(), Gender::Male)),
        Some((v, "female")) => Ok((v.trim().to_owned(), Gender::Female)),
        _ => Err(format!("bad voice entry {p:?} (want voice=male|female)")),
    }
}

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
            other => return Err(format!("LEXI_ATTEST_MODE: unknown mode {other:?}")),
        };
        // One key, or several separated by commas: one per friend (revoke one by removing it) or two during
        // a rotation (old and new accepted until every app is rebuilt).
        let attest_keys: Vec<Secret> = get("LEXI_ATTEST_SHARED_KEY")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(Secret::new)
            .collect();
        if attest_mode == AttestMode::SharedKey && attest_keys.is_empty() {
            return Err("LEXI_ATTEST_MODE=shared-key requires LEXI_ATTEST_SHARED_KEY".into());
        }

        let instructions = match get("LEXI_INSTRUCTIONS_FILE") {
            Some(path) => std::fs::read_to_string(&path)
                .map_err(|e| format!("LEXI_INSTRUCTIONS_FILE ({path}): {e}"))?,
            None => DEFAULT_INSTRUCTIONS.to_owned(),
        };

        let voice_genders = get("LEXI_VOICE_GENDERS")
            .unwrap_or_else(|| DEFAULT_VOICE_GENDERS.to_owned())
            .split(',')
            .filter(|p| !p.trim().is_empty())
            .map(parse_voice_gender)
            .collect::<Result<Vec<_>, _>>()?;
        let gemini_voices = get("LEXI_GEMINI_VOICES")
            .unwrap_or_else(|| DEFAULT_GEMINI_VOICES.to_owned())
            .split(',')
            .filter(|p| !p.trim().is_empty())
            .map(parse_voice_gender)
            .collect::<Result<Vec<_>, _>>()?;
        let gemini_voice = get("LEXI_GEMINI_VOICE").unwrap_or_else(|| "charon".into());
        if !gemini_voices.iter().any(|(v, _)| *v == gemini_voice) {
            return Err(format!(
                "LEXI_GEMINI_VOICE {gemini_voice:?} is not in LEXI_GEMINI_VOICES"
            ));
        }
        let gemini_search = match get("LEXI_GEMINI_SEARCH").as_deref().map(str::trim) {
            None | Some("" | "builtin") => GeminiSearch::Builtin,
            Some("off") => GeminiSearch::Off,
            Some(o) => return Err(format!("LEXI_GEMINI_SEARCH: unknown mode {o:?}")),
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
        let model = get("LEXI_MODEL").unwrap_or_else(|| "gpt-realtime-2.1-mini".into());
        let models: Vec<String> = get("LEXI_MODELS")
            .unwrap_or_else(|| DEFAULT_MODELS.to_owned())
            .split(',')
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .collect();
        if !models.contains(&model) {
            return Err(format!("LEXI_MODEL {model:?} is not in LEXI_MODELS"));
        }
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
            model,
            models,
            voice,
            google_api_key: get("GOOGLE_API_KEY")
                .filter(|s| !s.is_empty())
                .map(Secret::new),
            gemini_base_url: get("LEXI_GEMINI_BASE_URL")
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".into())
                .trim_end_matches('/')
                .to_owned(),
            gemini_model: get("LEXI_GEMINI_MODEL").unwrap_or_else(|| "gemini-3.8-live".into()),
            gemini_voices,
            gemini_voice,
            gemini_search,
            gemini_thinking: get("LEXI_GEMINI_THINKING").filter(|s| !s.trim().is_empty()),
            voices,
            instructions,
            voice_genders,
            reasoning_effort,
            vad_silence_ms: (vad_silence_ms != 0).then_some(vad_silence_ms),
            web_search: parse(&get, "LEXI_WEB_SEARCH", true)?,
            search_model: get("LEXI_SEARCH_MODEL").unwrap_or_else(|| "gpt-4.1-mini".into()),
            search_timeout: Duration::from_millis(parse(&get, "LEXI_SEARCH_TIMEOUT_MS", 12000)?),
            token_ttl_secs,
            rate_per_install: parse(&get, "LEXI_RATE_PER_INSTALL", 30)?,
            rate_per_ip: parse(&get, "LEXI_RATE_PER_IP", 120)?,
            rate_search_per_install: parse(&get, "LEXI_RATE_SEARCH_PER_INSTALL", 60)?,
            rate_search_per_ip: parse(&get, "LEXI_RATE_SEARCH_PER_IP", 240)?,
            attest_mode,
            attest_keys,
            trust_forwarded_for: parse(&get, "LEXI_TRUST_FORWARDED_FOR", false)?,
            upstream_timeout: Duration::from_millis(parse(&get, "LEXI_UPSTREAM_TIMEOUT_MS", 5000)?),
            request_timeout: Duration::from_millis(parse(&get, "LEXI_REQUEST_TIMEOUT_MS", 14000)?),
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
    fn built_in_prompt_matches_the_configmap_copy() {
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../deploy/k8s/base/instructions.txt");
        let deployed = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            DEFAULT_INSTRUCTIONS, deployed,
            "keep server/src/instructions.default.txt and {file:?} identical"
        );
    }

    #[test]
    fn built_in_prompt_has_the_rules_that_were_tested() {
        for needle in [
            "greet them with one short, warm sentence in Russian",
            "at most 20 words",
            "When the user stops you",
            "Examples of the style",
        ] {
            assert!(
                DEFAULT_INSTRUCTIONS.contains(needle)
                    || DEFAULT_INSTRUCTIONS
                        .to_lowercase()
                        .contains(&needle.to_lowercase()),
                "missing: {needle}"
            );
        }
        assert!(
            DEFAULT_INSTRUCTIONS.len() < 4000,
            "the prompt is paid for on every response: keep it short"
        );
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
