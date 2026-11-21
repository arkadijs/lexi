//! Upstream client for `POST /v1/realtime/client_secrets`.
//!
//! Request/response shape verified against the OpenAI API reference; see `minter/README.md`.

use crate::config::{Config, Secret};
use serde::Deserialize;
use serde_json::json;
use std::fmt;

#[derive(Debug)]
pub enum UpstreamError {
    /// Network failure, timeout, or 5xx after the retry.
    Unavailable,
    /// OpenAI answered with a non-retryable status (e.g. 401/403/429/400).
    Rejected(u16),
    /// 2xx but the body was not the documented shape.
    BadResponse,
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("upstream unavailable"),
            Self::Rejected(s) => write!(f, "upstream rejected request (status {s})"),
            Self::BadResponse => f.write_str("upstream returned an unexpected body"),
        }
    }
}

/// Ephemeral client secret. `Debug` is redacted so it cannot leak through logging.
pub struct ClientSecret {
    pub value: Secret,
    pub expires_at: i64,
}

impl fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientSecret")
            .field("value", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Deserialize)]
struct Response {
    value: String,
    expires_at: i64,
}

#[derive(Clone, Debug)]
pub struct OpenAiClient {
    http: reqwest::Client,
    url: String,
    api_key: Option<Secret>,
    model: String,
    instructions: String,
    default_voice: String,
    ttl_secs: u32,
    reasoning_effort: Option<String>,
    vad_silence_ms: Option<u32>,
}

impl OpenAiClient {
    pub fn new(cfg: &Config) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(cfg.upstream_timeout)
            .build()?;
        Ok(Self {
            http,
            url: format!("{}/v1/realtime/client_secrets", cfg.openai_base_url),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            instructions: cfg.instructions.clone(),
            default_voice: cfg.voice.clone(),
            ttl_secs: cfg.token_ttl_secs,
            reasoning_effort: cfg.reasoning_effort.clone(),
            vad_silence_ms: cfg.vad_silence_ms,
        })
    }

    /// Request body for `POST /v1/realtime/client_secrets` (shape verified against the API reference).
    fn body(&self, voice: &str) -> serde_json::Value {
        let mut turn_detection = json!({ "type": "server_vad", "interrupt_response": true });
        if let Some(ms) = self.vad_silence_ms {
            turn_detection["silence_duration_ms"] = ms.into();
        }
        let mut session = json!({
            "type": "realtime",
            "model": self.model,
            "instructions": self.instructions,
            "audio": {
                "output": { "voice": voice },
                "input": { "turn_detection": turn_detection }
            }
        });
        if let Some(effort) = &self.reasoning_effort {
            session["reasoning"] = json!({ "effort": effort });
        }
        json!({
            "expires_after": { "anchor": "created_at", "seconds": self.ttl_secs },
            "session": session
        })
    }

    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Mint one secret. One retry on connect/timeout errors and 5xx.
    /// `voice` must already be validated against the allowlist; `None` uses the configured default.
    pub async fn mint(
        &self,
        install_hash: &str,
        voice: Option<&str>,
    ) -> Result<ClientSecret, UpstreamError> {
        let Some(key) = &self.api_key else {
            return Err(UpstreamError::Unavailable);
        };
        let body = self.body(voice.unwrap_or(&self.default_voice));
        let mut last = UpstreamError::Unavailable;
        for _attempt in 0..2 {
            let sent = self
                .http
                .post(&self.url)
                .bearer_auth(key.expose())
                .header("OpenAI-Safety-Identifier", install_hash)
                .json(&body)
                .send()
                .await;
            let resp = match sent {
                Ok(r) => r,
                Err(_) => {
                    last = UpstreamError::Unavailable;
                    continue;
                }
            };
            let status = resp.status();
            if status.is_server_error() {
                last = UpstreamError::Unavailable;
                continue;
            }
            if !status.is_success() {
                return Err(UpstreamError::Rejected(status.as_u16()));
            }
            return match resp.json::<Response>().await {
                Ok(r) if !r.value.is_empty() => Ok(ClientSecret {
                    value: Secret::new(r.value),
                    expires_at: r.expires_at,
                }),
                _ => Err(UpstreamError::BadResponse),
            };
        }
        Err(last)
    }
}
