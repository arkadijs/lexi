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
    web_search: bool,
    search_model: String,
    search_url: String,
    search_timeout: std::time::Duration,
}

/// The one tool the bot has. The client runs it by calling `POST /v1/search` on the minter.
fn web_search_tool() -> serde_json::Value {
    json!({
        "type": "function",
        "name": "web_search",
        "description": "Search the web for current information: news, recent events, prices, scores, weather, \
    anything that may have changed after your training. Returns a short plain-text summary.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "A short, self-contained search query in English or the user's language." }
            },
            "required": ["query"]
        }
    })
}

/// Strip what a spoken answer cannot use: markdown links (keep the text), emphasis, headings, bullets and
/// bare URLs, then cut to at most `max` characters on a word boundary.
pub fn plain_speech(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('[') {
        // `[label](url)` -> `label`; a bracket without a link stays as it is.
        let after = &rest[i + 1..];
        match after
            .find("](")
            .and_then(|j| after[j + 2..].find(')').map(|k| (j, k)))
        {
            Some((j, k)) => {
                out.push_str(&rest[..i]);
                out.push_str(&after[..j]);
                rest = &after[j + 2 + k + 1..];
            }
            None => {
                out.push_str(&rest[..=i]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let cleaned: Vec<String> = out
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', '•']).trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.replace("**", "")
                .replace("__", "")
                .split_whitespace()
                .filter(|w| !w.starts_with("http://") && !w.starts_with("https://"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|l| !l.is_empty())
        .collect();
    let joined = cleaned.join(" ");
    if joined.chars().count() <= max {
        return joined;
    }
    let cut: String = joined.chars().take(max).collect();
    match cut.rfind([' ', '.']) {
        Some(i) if i > max / 2 => cut[..i].trim_end_matches([' ', ',', ';']).to_owned(),
        _ => cut,
    }
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
            web_search: cfg.web_search,
            search_model: cfg.search_model.clone(),
            search_url: format!("{}/v1/responses", cfg.openai_base_url),
            search_timeout: cfg.search_timeout,
        })
    }

    /// Request body for `POST /v1/realtime/client_secrets` (shape verified against the API reference).
    fn body(&self, model: &str, voice: &str) -> serde_json::Value {
        let mut turn_detection = json!({ "type": "server_vad", "interrupt_response": true });
        if let Some(ms) = self.vad_silence_ms {
            turn_detection["silence_duration_ms"] = ms.into();
        }
        let mut session = json!({
            "type": "realtime",
            "model": model,
            "instructions": self.instructions,
            "audio": {
                "output": { "voice": voice },
                "input": { "turn_detection": turn_detection }
            }
        });
        if self.web_search {
            session["tools"] = json!([web_search_tool()]);
            session["tool_choice"] = json!("auto");
        }
        if let Some(effort) = &self.reasoning_effort {
            session["reasoning"] = json!({ "effort": effort });
        }
        json!({
            "expires_after": { "anchor": "created_at", "seconds": self.ttl_secs },
            "session": session
        })
    }

    /// Run one web search through the Responses API and return a short plain-text answer.
    /// No retry: the caller is waiting on a spoken conversation, and the model can say it failed.
    pub async fn search(&self, install_hash: &str, query: &str) -> Result<String, UpstreamError> {
        let Some(key) = &self.api_key else {
            return Err(UpstreamError::Unavailable);
        };
        let body = json!({
            "model": self.search_model,
            "tools": [{ "type": "web_search", "search_context_size": "low" }],
            "instructions": "Answer the question from current web results in at most two short sentences \
        (under 35 words). Plain text only: no markdown, no lists, no links, no source names. Give the date when it matters.",
            "input": query,
        });
        let resp = self
            .http
            .post(&self.search_url)
            .timeout(self.search_timeout)
            .bearer_auth(key.expose())
            .header("OpenAI-Safety-Identifier", install_hash)
            .json(&body)
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
        let text = v["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|o| o["type"] == "message")
            .flat_map(|o| o["content"].as_array().into_iter().flatten())
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let text = plain_speech(&text, 350);
        if text.is_empty() {
            Err(UpstreamError::BadResponse)
        } else {
            Ok(text)
        }
    }

    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Mint one secret. One retry on connect/timeout errors and 5xx.
    /// `voice` and `model` must already be validated against their allowlists; `None` uses the configured default.
    pub async fn mint(
        &self,
        install_hash: &str,
        voice: Option<&str>,
        model: Option<&str>,
    ) -> Result<ClientSecret, UpstreamError> {
        let Some(key) = &self.api_key else {
            return Err(UpstreamError::Unavailable);
        };
        let body = self.body(
            model.unwrap_or(&self.model),
            voice.unwrap_or(&self.default_voice),
        );
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

#[cfg(test)]
mod tests {
    use super::plain_speech;

    #[test]
    fn strips_markdown_links_emphasis_bullets_and_urls() {
        let t = "## News\n- **Turkey**: draft ([apnews.com](https://apnews.com/a?x=1)).\n* See https://x.org/y now\n\nDone [1] ok";
        assert_eq!(
            plain_speech(t, 500),
            "News Turkey: draft (apnews.com). See now Done [1] ok"
        );
    }

    #[test]
    fn cuts_on_a_word_boundary() {
        let t = "one two three four five six seven eight nine ten";
        let c = plain_speech(t, 22);
        assert_eq!(c, "one two three four");
        assert!(plain_speech("short", 22) == "short");
    }
}
