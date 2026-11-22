//! Data-channel (`oai-events`) server events. Unknown events and fields are ignored.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "session.created")]
    SessionCreated,
    #[serde(rename = "session.updated")]
    SessionUpdated,
    /// The user started speaking: flush local playout (barge-in).
    #[serde(rename = "input_audio_buffer.speech_started")]
    SpeechStarted {
        #[serde(default)]
        audio_start_ms: Option<u64>,
    },
    #[serde(rename = "input_audio_buffer.speech_stopped")]
    SpeechStopped,
    #[serde(rename = "response.done")]
    ResponseDone { response: ResponseSummary },
    #[serde(rename = "error")]
    Error { error: ApiError },
    /// Any event type we do not handle.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ResponseSummary {
    #[serde(default)]
    pub id: Option<String>,
    /// `completed`, `cancelled`, `failed`, `incomplete`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
    /// Output items; only function calls are looked at.
    #[serde(default)]
    pub output: Vec<OutputItem>,
}

/// One item of `response.output`. For `type == "function_call"`: the tool `name`, its `call_id` and the
/// JSON-encoded `arguments`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct OutputItem {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

impl ResponseSummary {
    /// `(call_id, name, arguments)` of every function call in the response.
    pub fn function_calls(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.output
            .iter()
            .filter(|i| i.kind == "function_call")
            .filter_map(|i| {
                Some((
                    i.call_id.as_deref()?,
                    i.name.as_deref()?,
                    i.arguments.as_deref().unwrap_or("{}"),
                ))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub input_token_details: TokenDetails,
    #[serde(default)]
    pub output_token_details: TokenDetails,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct TokenDetails {
    #[serde(default)]
    pub text_tokens: u64,
    #[serde(default)]
    pub audio_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    /// Input only. Absent on output details.
    #[serde(default)]
    pub image_tokens: u64,
    /// Input only: the cached subset of each modality (cached tokens are counted inside the totals above).
    #[serde(default)]
    pub cached_tokens_details: CachedDetails,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct CachedDetails {
    #[serde(default)]
    pub text_tokens: u64,
    #[serde(default)]
    pub audio_tokens: u64,
    #[serde(default)]
    pub image_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ApiError {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Parse one data-channel message. Malformed JSON yields `None`; unknown types yield `Other`.
pub fn parse_event(text: &str) -> Option<ServerEvent> {
    serde_json::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_events() {
        assert_eq!(
            parse_event(r#"{"type":"session.created","session":{"id":"x"}}"#),
            Some(ServerEvent::SessionCreated)
        );
        assert_eq!(
            parse_event(
                r#"{"type":"input_audio_buffer.speech_started","audio_start_ms":1200,"item_id":"i"}"#
            ),
            Some(ServerEvent::SpeechStarted {
                audio_start_ms: Some(1200)
            })
        );
        assert_eq!(
            parse_event(r#"{"type":"input_audio_buffer.speech_started"}"#),
            Some(ServerEvent::SpeechStarted {
                audio_start_ms: None
            })
        );
    }

    #[test]
    fn response_done_carries_usage() {
        let j = r#"{"type":"response.done","event_id":"e","response":{"id":"resp_1","status":"completed",
            "output":[{"type":"message"}],
            "usage":{"total_tokens":150,"input_tokens":100,"output_tokens":50,
              "input_token_details":{"text_tokens":40,"audio_tokens":60,"cached_tokens":0,"extra":1},
              "output_token_details":{"text_tokens":10,"audio_tokens":40}}}}"#;
        let Some(ServerEvent::ResponseDone { response }) = parse_event(j) else {
            panic!("not response.done")
        };
        assert_eq!(response.status.as_deref(), Some("completed"));
        let u = response.usage.unwrap();
        assert_eq!(
            (u.total_tokens, u.input_tokens, u.output_tokens),
            (150, 100, 50)
        );
        assert_eq!(
            (
                u.input_token_details.audio_tokens,
                u.output_token_details.audio_tokens
            ),
            (60, 40)
        );
    }

    #[test]
    fn response_done_lists_function_calls() {
        let j = r#"{"type":"response.done","response":{"status":"completed","output":[
            {"type":"message","content":[]},
            {"type":"function_call","name":"web_search","call_id":"call_1","arguments":"{\"query\":\"news\"}"}]}}"#;
        let Some(ServerEvent::ResponseDone { response }) = parse_event(j) else {
            panic!("not response.done")
        };
        let calls: Vec<_> = response.function_calls().collect();
        assert_eq!(calls, vec![("call_1", "web_search", r#"{"query":"news"}"#)]);
    }

    #[test]
    fn parses_image_and_cached_details() {
        let j = r#"{"type":"response.done","response":{"usage":{"total_tokens":253,"input_tokens":132,
          "output_tokens":121,"input_token_details":{"text_tokens":119,"audio_tokens":13,"image_tokens":0,
          "cached_tokens":64,"cached_tokens_details":{"text_tokens":64,"audio_tokens":0,"image_tokens":0}},
          "output_token_details":{"text_tokens":30,"audio_tokens":91}}}}"#;
        let Some(ServerEvent::ResponseDone { response }) = parse_event(j) else {
            panic!()
        };
        let u = response.usage.unwrap();
        assert_eq!(u.input_token_details.cached_tokens_details.text_tokens, 64);
        assert_eq!(u.input_token_details.image_tokens, 0);
        assert_eq!(u.output_token_details.audio_tokens, 91);
    }

    #[test]
    fn response_done_without_usage_is_fine() {
        let Some(ServerEvent::ResponseDone { response }) =
            parse_event(r#"{"type":"response.done","response":{"status":"cancelled"}}"#)
        else {
            panic!()
        };
        assert!(response.usage.is_none());
    }

    #[test]
    fn error_event() {
        let Some(ServerEvent::Error { error }) = parse_event(
            r#"{"type":"error","error":{"type":"invalid_request_error","code":"bad","message":"nope","param":null}}"#,
        ) else {
            panic!()
        };
        assert_eq!(error.code.as_deref(), Some("bad"));
    }

    #[test]
    fn unknown_and_malformed_are_ignored() {
        assert_eq!(
            parse_event(r#"{"type":"rate_limits.updated","rate_limits":[]}"#),
            Some(ServerEvent::Other)
        );
        assert_eq!(
            parse_event(r#"{"type":"response.output_audio.delta","delta":"AAAA"}"#),
            Some(ServerEvent::Other)
        );
        assert_eq!(parse_event("not json"), None);
        assert_eq!(parse_event(r#"{"no_type":1}"#), None);
    }
}
