//! OpenAI Realtime glue: ephemeral-token fetch, SDP exchange, data-channel events.
//!
//! ## Verified against the current OpenAI docs (2026-11-21)
//!
//! Only the **Realtime** flow is used (token-billed, model `gpt-realtime-2.1-mini`). The newer
//! "GPT-Live" API (`POST /v1/live/sessions`, per-minute billing) is deliberately NOT used.
//!
//! * `POST https://api.openai.com/v1/realtime/calls`, `Authorization: Bearer <ek_...>`,
//!   `Content-Type: application/sdp`, body = SDP offer, response body = SDP answer
//!   (`Content-Type: application/sdp`, HTTP 201). A `Location` header may be present.
//! * Data channel label: `oai-events`, created by the client before the offer.
//! * Server events handled: `session.created`, `session.updated`,
//!   `input_audio_buffer.speech_started` (barge-in), `response.done` (carries `usage`), `error`.
//!   Everything else is parsed as [`ServerEvent::Other`] and ignored.
//! * Ephemeral secrets come from our minter (`POST /v1/call-token`) or, for local development
//!   only, directly from `POST /v1/realtime/client_secrets` using a dev API key from the
//!   environment ([`mint_with_dev_key`]).
//!
//! Tokens and keys are held in [`Secret`], whose `Debug` is redacted. Never log them.

pub mod attest;
pub mod events;

pub use events::{parse_event, ServerEvent, Usage};

use serde::Deserialize;
use std::fmt;
use std::time::Duration;

pub const OPENAI_BASE: &str = "https://api.openai.com";
pub const MODEL: &str = "gpt-realtime-2.1-mini";
pub const DATA_CHANNEL_LABEL: &str = "oai-events";
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);
/// A web search takes 5-11 s end to end.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(16);

/// A string that never prints itself.
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

#[derive(Debug)]
pub enum OpenAiError {
    /// Transport failure (DNS, connect, TLS, timeout).
    Network(String),
    /// Non-2xx HTTP status.
    Status(u16),
    /// 2xx with a body we could not use.
    BadResponse(&'static str),
}

impl fmt::Display for OpenAiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Status(s) => write!(f, "unexpected HTTP status {s}"),
            Self::BadResponse(w) => write!(f, "bad response: {w}"),
        }
    }
}
impl std::error::Error for OpenAiError {}

fn map_err(e: ureq::Error) -> OpenAiError {
    match e {
        ureq::Error::StatusCode(s) => OpenAiError::Status(s),
        // Display of ureq errors never includes request headers or bodies.
        other => OpenAiError::Network(other.to_string()),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

#[derive(Debug)]
pub struct MintedToken {
    pub token: Secret,
    pub expires_at: i64,
    pub model: String,
    /// `openai` (WebRTC) or `google` (Gemini Live WebSocket).
    pub provider: String,
}

#[derive(Deserialize)]
struct MinterResponse {
    token: String,
    expires_at: i64,
    model: String,
    #[serde(default)]
    provider: Option<String>,
}

/// Ask the minter for an ephemeral key. `install_hash` is the SHA-256 hex of the install ID;
/// `voice` and `model` are among the minter's allowed values (`None` = the minter's default).
pub fn mint_from_minter(
    minter_url: &str,
    install_hash: &str,
    attest: Option<&str>,
    voice: Option<&str>,
    model: Option<&str>,
    provider: Option<&str>,
) -> Result<MintedToken, OpenAiError> {
    let url = format!("{}/v1/call-token", minter_url.trim_end_matches('/'));
    let mut req = agent().post(&url).header("X-Lexi-Install", install_hash);
    if let Some(a) = attest {
        req = req.header("X-Lexi-Attest", a);
    }
    let mut body = serde_json::Map::new();
    if let Some(v) = voice {
        body.insert("voice".into(), v.into());
    }
    if let Some(m) = model {
        body.insert("model".into(), m.into());
    }
    if let Some(p) = provider {
        body.insert("provider".into(), p.into());
    }
    let body = serde_json::Value::Object(body).to_string();
    let mut resp = req
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(map_err)?;
    let r: MinterResponse = resp
        .body_mut()
        .read_json()
        .map_err(|_| OpenAiError::BadResponse("minter JSON"))?;
    if r.token.is_empty() {
        return Err(OpenAiError::BadResponse("empty token"));
    }
    Ok(MintedToken {
        token: Secret::new(r.token),
        expires_at: r.expires_at,
        model: r.model,
        provider: r.provider.unwrap_or_else(|| "openai".into()),
    })
}

#[derive(Deserialize)]
struct SearchResponse {
    text: String,
}

/// Ask the minter to run a web search (`POST /v1/search`) and return the short plain-text answer.
pub fn search_via_minter(
    minter_url: &str,
    install_hash: &str,
    attest: Option<&str>,
    query: &str,
) -> Result<String, OpenAiError> {
    let url = format!("{}/v1/search", minter_url.trim_end_matches('/'));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(SEARCH_TIMEOUT))
        .build()
        .into();
    let mut req = agent.post(&url).header("X-Lexi-Install", install_hash);
    if let Some(a) = attest {
        req = req.header("X-Lexi-Attest", a);
    }
    let body = serde_json::json!({ "query": query }).to_string();
    let mut resp = req
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(map_err)?;
    let r: SearchResponse = resp
        .body_mut()
        .read_json()
        .map_err(|_| OpenAiError::BadResponse("search JSON"))?;
    Ok(r.text)
}

#[derive(Deserialize)]
struct SecretResponse {
    value: String,
    expires_at: i64,
}

/// Session options for [`mint_with_dev_key`] (development and measurement only; deployed sessions are
/// configured by the minter). `None` keeps the API default.
#[derive(Debug, Clone)]
pub struct DevSession {
    pub model: String,
    pub voice: String,
    pub instructions: String,
    /// server_vad `silence_duration_ms` (API default 500).
    pub silence_ms: Option<u32>,
    /// Use `semantic_vad` with this eagerness (`low`/`medium`/`high`/`auto`) instead of server_vad.
    pub eagerness: Option<String>,
    /// `session.reasoning.effort` (`minimal`/`low`/`medium`/`high`/`xhigh`).
    pub reasoning_effort: Option<String>,
}

impl Default for DevSession {
    fn default() -> Self {
        Self {
            model: MODEL.into(),
            voice: "cedar".into(),
            instructions: "You are Lexi, a friendly, concise vocabulary coach. Keep replies to one to three sentences.".into(),
            silence_ms: None,
            eagerness: None,
            reasoning_effort: None,
        }
    }
}

impl DevSession {
    fn body(&self) -> serde_json::Value {
        let turn_detection = match &self.eagerness {
            Some(e) => {
                serde_json::json!({ "type": "semantic_vad", "eagerness": e, "interrupt_response": true })
            }
            None => {
                let mut t = serde_json::json!({ "type": "server_vad", "interrupt_response": true });
                if let Some(ms) = self.silence_ms {
                    t["silence_duration_ms"] = ms.into();
                }
                t
            }
        };
        let mut session = serde_json::json!({
            "type": "realtime",
            "model": self.model,
            "instructions": self.instructions,
            "audio": {
                "output": { "voice": self.voice },
                "input": { "turn_detection": turn_detection }
            }
        });
        if let Some(effort) = &self.reasoning_effort {
            session["reasoning"] = serde_json::json!({ "effort": effort });
        }
        serde_json::json!({ "expires_after": { "anchor": "created_at", "seconds": 60 }, "session": session })
    }
}

/// Development only: mint directly with an API key (from the environment, never a file).
pub fn mint_with_dev_key(
    base: &str,
    api_key: &Secret,
    install_hash: &str,
    opts: &DevSession,
) -> Result<MintedToken, OpenAiError> {
    let body = opts.body();
    let url = format!("{}/v1/realtime/client_secrets", base.trim_end_matches('/'));
    let mut resp = agent()
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key.expose()))
        .header("OpenAI-Safety-Identifier", install_hash)
        .send_json(&body)
        .map_err(map_err)?;
    let r: SecretResponse = resp
        .body_mut()
        .read_json()
        .map_err(|_| OpenAiError::BadResponse("client_secrets JSON"))?;
    Ok(MintedToken {
        token: Secret::new(r.value),
        expires_at: r.expires_at,
        model: opts.model.clone(),
        provider: "openai".into(),
    })
}

/// POST the SDP offer to `<base>/v1/realtime/calls` and return the SDP answer.
pub fn exchange_sdp(base: &str, token: &Secret, offer_sdp: &str) -> Result<String, OpenAiError> {
    let url = format!("{}/v1/realtime/calls", base.trim_end_matches('/'));
    let mut resp = agent()
        .post(&url)
        .header("Authorization", format!("Bearer {}", token.expose()))
        .header("Content-Type", "application/sdp")
        .send(offer_sdp)
        .map_err(map_err)?;
    let answer = resp
        .body_mut()
        .read_to_string()
        .map_err(|_| OpenAiError::BadResponse("SDP body"))?;
    if !answer.starts_with("v=0") {
        return Err(OpenAiError::BadResponse("answer is not SDP"));
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// One-shot HTTP server: returns the raw request text and replies with `response`.
    fn serve_once(response: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 8192];
            let mut got = 0;
            // read headers + small body
            loop {
                let n = s.read(&mut buf[got..]).unwrap();
                got += n;
                let text = String::from_utf8_lossy(&buf[..got]).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let cl = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if got >= i + 4 + cl {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            s.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf[..got]).to_string()
        });
        (url, h)
    }

    #[test]
    fn secret_debug_is_redacted() {
        assert!(!format!("{:?}", Secret::new("ek_abc")).contains("ek_abc"));
        let m = MintedToken {
            token: Secret::new("ek_zzz"),
            expires_at: 1,
            model: "m".into(),
            provider: "openai".into(),
        };
        assert!(!format!("{m:?}").contains("ek_zzz"));
    }

    #[test]
    fn sdp_exchange_sends_documented_request() {
        let (url, h) = serve_once("HTTP/1.1 201 Created\r\nContent-Type: application/sdp\r\nContent-Length: 11\r\n\r\nv=0\r\ns=ans\n");
        let ans = exchange_sdp(&url, &Secret::new("ek_test"), "v=0\r\ns=offer\r\n").unwrap();
        assert!(ans.starts_with("v=0"));
        let req = h.join().unwrap().to_ascii_lowercase();
        assert!(req.starts_with("post /v1/realtime/calls "));
        assert!(req.contains("authorization: bearer ek_test"));
        assert!(req.contains("content-type: application/sdp"));
        assert!(req.ends_with("s=offer\r\n"));
    }

    #[test]
    fn sdp_exchange_maps_errors() {
        let (url, h) = serve_once("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let e = exchange_sdp(&url, &Secret::new("bad"), "v=0\r\n").unwrap_err();
        h.join().unwrap();
        assert!(matches!(e, OpenAiError::Status(401)), "{e}");
        assert!(!e.to_string().contains("bad"));

        let (url, h) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");
        let e = exchange_sdp(&url, &Secret::new("t"), "v=0\r\n").unwrap_err();
        h.join().unwrap();
        assert!(matches!(e, OpenAiError::BadResponse(_)));
    }

    #[test]
    fn minter_fetch_parses_response_and_sends_install_header() {
        let body = r#"{"token":"ek_x","expires_at":123,"model":"gpt-realtime-2.1-mini"}"#;
        let resp: &'static str = Box::leak(
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_boxed_str(),
        );
        let (url, h) = serve_once(resp);
        let t = mint_from_minter(&url, &"a".repeat(64), None, None, None, None).unwrap();
        assert_eq!(
            (t.token.expose(), t.expires_at, t.model.as_str()),
            ("ek_x", 123, "gpt-realtime-2.1-mini")
        );
        let req = h.join().unwrap().to_ascii_lowercase();
        assert!(req.starts_with("post /v1/call-token "));
        assert!(req.contains(&format!("x-lexi-install: {}", "a".repeat(64))));
        assert!(req.ends_with("{}"), "no voice requested: empty JSON object");
    }

    #[test]
    fn minter_fetch_sends_the_requested_voice() {
        let body = r#"{"token":"ek_x","expires_at":1,"model":"gpt-realtime-2.1-mini"}"#;
        let resp: &'static str = Box::leak(
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_boxed_str(),
        );
        let (url, h) = serve_once(resp);
        mint_from_minter(
            &url,
            &"b".repeat(64),
            Some("x-att"),
            Some("cedar"),
            Some("gpt-realtime-2.1"),
            Some("google"),
        )
        .unwrap();
        let req = h.join().unwrap().to_ascii_lowercase();
        assert!(req.contains("content-type: application/json"));
        assert!(req.contains("x-lexi-attest: x-att"));
        let json: serde_json::Value =
            serde_json::from_str(req.rsplit("\r\n\r\n").next().unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"voice":"cedar","model":"gpt-realtime-2.1","provider":"google"}),
            "{req}"
        );
    }
}

#[cfg(test)]
mod dev_session_tests {
    use super::*;

    #[test]
    fn default_body_matches_the_documented_shape() {
        let b = DevSession::default().body();
        assert_eq!(b["session"]["model"], "gpt-realtime-2.1-mini");
        assert_eq!(b["session"]["audio"]["output"]["voice"], "cedar");
        assert_eq!(
            b["session"]["audio"]["input"]["turn_detection"]["type"],
            "server_vad"
        );
        assert!(b["session"].get("reasoning").is_none());
        assert!(b["session"]["audio"]["input"]["turn_detection"]
            .get("silence_duration_ms")
            .is_none());
    }

    #[test]
    fn options_are_applied() {
        let b = DevSession {
            voice: "cedar".into(),
            silence_ms: Some(250),
            reasoning_effort: Some("minimal".into()),
            ..DevSession::default()
        }
        .body();
        assert_eq!(b["session"]["audio"]["output"]["voice"], "cedar");
        assert_eq!(
            b["session"]["audio"]["input"]["turn_detection"]["silence_duration_ms"],
            250
        );
        assert_eq!(b["session"]["reasoning"]["effort"], "minimal");
        let s = DevSession {
            eagerness: Some("high".into()),
            ..DevSession::default()
        }
        .body();
        assert_eq!(
            s["session"]["audio"]["input"]["turn_detection"]["type"],
            "semantic_vad"
        );
        assert_eq!(
            s["session"]["audio"]["input"]["turn_detection"]["eagerness"],
            "high"
        );
    }
}
