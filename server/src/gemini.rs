//! Google Gemini Live: mint a one-use ephemeral token whose configuration is locked server-side
//! (`POST /v1beta/auth_tokens`, field `bidiGenerateContentSetup`; verified 2026-11-22 against the live
//! API — the documented `liveConnectConstraints` is rejected). The app then opens the WebSocket
//! `BidiGenerateContentConstrained?access_token=<token>` itself and sends an empty `setup`.

use crate::config::{Config, GeminiSearch, Gender, Secret};
use crate::openai::{ClientSecret, UpstreamError};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

/// A new session must start within this many seconds of minting.
const START_WITHIN_SECS: i64 = 60;
/// Token lifetime for sending messages (Google's default is 30 minutes; audio sessions last up to 15).
const LIFETIME_SECS: i64 = 1800;

#[derive(Clone, Debug)]
pub struct GeminiClient {
    http: reqwest::Client,
    url: String,
    api_key: Option<Secret>,
    model: String,
    instructions: String,
    voices: Vec<(String, Gender)>,
    thinking: Option<String>,
    vad_silence_ms: Option<u32>,
    web_search: GeminiSearch,
}

/// `kore` -> `Kore` (Google's voice names are capitalized).
fn api_voice_name(v: &str) -> String {
    let mut c = v.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// RFC 3339 UTC (`2026-11-22T10:15:00Z`) for a unix time; no date library needed.
pub fn rfc3339(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

impl GeminiClient {
    pub fn new(cfg: &Config) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(cfg.upstream_timeout)
            .build()?;
        Ok(Self {
            http,
            url: format!("{}/v1beta/auth_tokens", cfg.gemini_base_url),
            api_key: cfg.gemini_api_key.clone(),
            model: cfg.gemini_model.clone(),
            instructions: cfg.instructions.clone(),
            voices: cfg.gemini_voices.clone(),
            thinking: cfg.gemini_thinking.clone(),
            vad_silence_ms: cfg.vad_silence_ms,
            web_search: if cfg.web_search {
                cfg.gemini_search
            } else {
                GeminiSearch::Off
            },
        })
    }

    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The locked session setup the app cannot change: model, voice, instructions (with the voice's gender
    /// line), audio output, our `web_search` function and, if configured, thinking level and VAD silence.
    fn setup(&self, voice: &str) -> serde_json::Value {
        let instructions = match self.voices.iter().find(|(v, _)| v == voice) {
            Some((_, g)) => format!("{}\n\n{}", self.instructions.trim_end(), g.instruction()),
            None => self.instructions.clone(),
        };
        let mut generation = json!({
            "responseModalities": ["AUDIO"],
            "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": api_voice_name(voice) } } }
        });
        if let Some(level) = &self.thinking {
            generation["thinkingConfig"] = json!({ "thinkingLevel": level });
        }
        let mut setup = json!({
            "model": format!("models/{}", self.model),
            "generationConfig": generation,
            "systemInstruction": { "parts": [{ "text": instructions }] },
        });
        match self.web_search {
            // Google's own search inside the Live session: no function round trip, ~1.6 s to the answer.
            GeminiSearch::Builtin => setup["tools"] = json!([{ "googleSearch": {} }]),
            GeminiSearch::Off => {}
        }
        if let Some(ms) = self.vad_silence_ms {
            setup["realtimeInputConfig"] =
                json!({ "automaticActivityDetection": { "silenceDurationMs": ms } });
        }
        setup
    }

    fn body(&self, voice: &str, now_unix: i64) -> serde_json::Value {
        json!({
            "uses": 1,
            "expireTime": rfc3339(now_unix + LIFETIME_SECS),
            "newSessionExpireTime": rfc3339(now_unix + START_WITHIN_SECS),
            "bidiGenerateContentSetup": self.setup(voice),
        })
    }

    /// Mint one token. `voice` must already be validated against the allowlist.
    pub async fn mint(&self, voice: &str) -> Result<ClientSecret, UpstreamError> {
        let Some(key) = &self.api_key else {
            return Err(UpstreamError::Unavailable);
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let resp = self
            .http
            .post(&self.url)
            .header("x-goog-api-key", key.expose())
            .json(&self.body(voice, now))
            .send()
            .await
            .map_err(|_| UpstreamError::Unavailable)?;
        let status = resp.status();
        if status.is_server_error() {
            return Err(UpstreamError::Unavailable);
        }
        if !status.is_success() {
            return Err(UpstreamError::Rejected(status.as_u16()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|_| UpstreamError::BadResponse)?;
        match v["name"].as_str().filter(|n| !n.is_empty()) {
            Some(name) => Ok(ClientSecret {
                value: Secret::new(name),
                expires_at: now + START_WITHIN_SECS,
            }),
            None => Err(UpstreamError::BadResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_795_340_100), "2026-11-22T09:35:00Z");
    }

    #[test]
    fn voice_names_are_capitalized() {
        assert_eq!(api_voice_name("kore"), "Kore");
        assert_eq!(api_voice_name(""), "");
    }
}
