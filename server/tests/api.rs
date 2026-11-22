#![allow(clippy::unwrap_used)]
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lexi::attest::sign;
use lexi::config::{Config, Secret};
use lexi::{app, AppState};
use std::net::SocketAddr;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "sk-test-SECRETKEY-1234567890";
const TOKEN: &str = "ek_test_TOKENVALUE_abcdef";
const INSTALL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn config(upstream: &str, extra: &[(&str, &str)]) -> Config {
    let mut env: Vec<(String, String)> = vec![
        ("OPENAI_API_KEY".into(), API_KEY.into()),
        ("LEXI_OPENAI_BASE_URL".into(), upstream.into()),
        ("LEXI_UPSTREAM_TIMEOUT_MS".into(), "300".into()),
    ];
    env.extend(extra.iter().map(|(k, v)| ((*k).into(), (*v).into())));
    Config::from_lookup(|k| {
        env.iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    })
    .unwrap()
}

fn router(cfg: Config) -> axum::Router {
    app(AppState::new(cfg).unwrap())
}

fn post(install: Option<&str>, attest: Option<&str>, ip: &str) -> Request<Body> {
    post_body("{}", install, attest, ip)
}

fn post_body(body: &str, install: Option<&str>, attest: Option<&str>, ip: &str) -> Request<Body> {
    let mut b = Request::post("/v1/call-token");
    if let Some(i) = install {
        b = b.header("x-lexi-install", i);
    }
    if let Some(a) = attest {
        b = b.header("x-lexi-attest", a);
    }
    let mut req = b.body(Body::from(body.to_owned())).unwrap();
    let addr: SocketAddr = format!("{ip}:5555").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn send(r: &axum::Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = r.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn ok_upstream() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(
        serde_json::json!({"value": TOKEN, "expires_at": 1_900_000_000, "session": {}}),
    )
}

#[tokio::test]
async fn success_returns_token_and_sends_documented_upstream_request() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/realtime/client_secrets"))
        .and(header(
            "authorization",
            format!("Bearer {API_KEY}").as_str(),
        ))
        .and(header("openai-safety-identifier", INSTALL))
        .and(body_partial_json(serde_json::json!({
            "expires_after": {"anchor": "created_at", "seconds": 60},
            "session": {"type": "realtime", "model": "gpt-realtime-2.1-mini",
                        "reasoning": {"effort": "low"},
                        "audio": {"output": {"voice": "cedar"},
                                  "input": {"turn_detection": {"type": "server_vad",
                                                               "silence_duration_ms": 400,
                                                               "interrupt_response": true}}}}
        })))
        .respond_with(ok_upstream())
        .expect(1)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, _, body) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["token"], TOKEN);
    assert_eq!(v["expires_at"], 1_900_000_000);
    assert_eq!(v["model"], "gpt-realtime-2.1-mini");
}

#[tokio::test]
async fn upstream_4xx_maps_to_502_without_retry() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, _, body) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(!body.contains("bad key"));
}

#[tokio::test]
async fn upstream_5xx_is_retried_once_then_502() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(2)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn upstream_5xx_then_success_recovers() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&up)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn upstream_timeout_maps_to_502() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream().set_delay(Duration::from_secs(2)))
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn malformed_upstream_body_maps_to_502() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"nope\":1}"))
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn per_install_rate_limit_returns_429_with_retry_after() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[("LEXI_RATE_PER_INSTALL", "2")]));
    for _ in 0..2 {
        let (s, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, h, _) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = h["retry-after"].to_str().unwrap().parse().unwrap();
    assert!(retry >= 1);
    let other = "f".repeat(64);
    let (s, ..) = send(&r, post(Some(&other), None, "10.0.0.1")).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn per_ip_rate_limit_returns_429() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[("LEXI_RATE_PER_IP", "1")]));
    let (s, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(s, StatusCode::OK);
    let (s, h, _) = send(&r, post(Some(&"a".repeat(64)), None, "10.0.0.1")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(h.contains_key("retry-after"));
    let (s, ..) = send(&r, post(Some(&"b".repeat(64)), None, "10.0.0.2")).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn bad_attestation_is_401_and_never_reaches_upstream() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .expect(0)
        .mount(&up)
        .await;
    let r = router(config(
        &up.uri(),
        &[
            ("LEXI_ATTEST_MODE", "shared-key"),
            ("LEXI_ATTEST_SHARED_KEY", "k"),
        ],
    ));
    let (s, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, ..) = send(&r, post(Some(INSTALL), Some("1.00"), "10.0.0.1")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let wrong = sign(
        &Secret::new("wrong"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let (s, ..) = send(&r, post(Some(INSTALL), Some(&wrong), "10.0.0.1")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn good_attestation_is_accepted() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up)
        .await;
    let r = router(config(
        &up.uri(),
        &[
            ("LEXI_ATTEST_MODE", "shared-key"),
            ("LEXI_ATTEST_SHARED_KEY", "k"),
        ],
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let h = sign(&Secret::new("k"), now).unwrap();
    let (s, ..) = send(&r, post(Some(INSTALL), Some(&h), "10.0.0.1")).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn missing_or_invalid_install_header_is_400() {
    let up = MockServer::start().await;
    let r = router(config(&up.uri(), &[]));
    let (s, ..) = send(&r, post(None, None, "10.0.0.1")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, ..) = send(&r, post(Some("not-a-hash"), None, "10.0.0.1")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn health_and_readiness() {
    let up = MockServer::start().await;
    let r = router(config(&up.uri(), &[]));
    let get = |p: &str| Request::get(p).body(Body::empty()).unwrap();
    assert_eq!(send(&r, get("/healthz")).await.0, StatusCode::OK);
    assert_eq!(send(&r, get("/readyz")).await.0, StatusCode::OK);

    // No provider key at all: still alive and ready (the deployment must not fail), but every call is rejected
    // with 501 "provider not configured" and the readiness body says so.
    let mut cfg = config(&up.uri(), &[]);
    cfg.api_key = None;
    let r = router(cfg);
    assert_eq!(send(&r, get("/healthz")).await.0, StatusCode::OK);
    let (s, _, body) = send(&r, get("/readyz")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("no provider configured"), "{body}");
    assert_eq!(
        send(&r, post(Some(INSTALL), None, "10.0.0.1")).await.0,
        StatusCode::NOT_IMPLEMENTED
    );
    assert_eq!(
        send(
            &r,
            post_body(r#"{"provider":"google"}"#, Some(INSTALL), None, "10.0.0.1")
        )
        .await
        .0,
        StatusCode::NOT_IMPLEMENTED
    );
}

#[tokio::test]
async fn requested_voice_is_forwarded_upstream() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({
            "session": {"audio": {"output": {"voice": "cedar"}}}
        })))
        .respond_with(ok_upstream())
        .expect(1)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, ..) = send(
        &r,
        post_body(r#"{"voice":"cedar"}"#, Some(INSTALL), None, "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unknown_voice_is_rejected_without_calling_upstream() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .expect(0)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    for body in [
        r#"{"voice":"hal9000"}"#,
        r#"{"voice":""}"#,
        r#"{"voice":"MARIN"}"#,
    ] {
        let (status, _, text) = send(&r, post_body(body, Some(INSTALL), None, "10.0.0.1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(text.contains("unknown voice"));
    }
}

#[tokio::test]
async fn voice_allowlist_is_configurable() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[("LEXI_VOICES", "marin,cedar")]));
    let (ok, ..) = send(
        &r,
        post_body(r#"{"voice":"cedar"}"#, Some(INSTALL), None, "10.0.0.1"),
    )
    .await;
    let (no, ..) = send(
        &r,
        post_body(r#"{"voice":"coral"}"#, Some(INSTALL), None, "10.0.0.1"),
    )
    .await;
    assert_eq!((ok, no), (StatusCode::OK, StatusCode::BAD_REQUEST));
}

#[tokio::test]
async fn requested_model_is_forwarded_and_echoed() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({
            "session": {"model": "gpt-realtime-2.1"}
        })))
        .respond_with(ok_upstream())
        .expect(1)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, _, text) = send(
        &r,
        post_body(
            r#"{"model":"gpt-realtime-2.1"}"#,
            Some(INSTALL),
            None,
            "10.0.0.1",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains(r#""model":"gpt-realtime-2.1""#), "{text}");
}

#[tokio::test]
async fn unknown_model_is_rejected_without_calling_upstream() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .expect(0)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    for body in [r#"{"model":"gpt-live-1"}"#, r#"{"model":""}"#] {
        let (status, _, text) = send(&r, post_body(body, Some(INSTALL), None, "10.0.0.1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(text.contains("unknown model"));
    }
}

fn search_req(body: &str, install: Option<&str>, ip: &str) -> Request<Body> {
    let mut b = Request::post("/v1/search");
    if let Some(i) = install {
        b = b.header("x-lexi-install", i);
    }
    let mut req = b.body(Body::from(body.to_owned())).unwrap();
    let addr: SocketAddr = format!("{ip}:5555").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

#[tokio::test]
async fn session_gets_the_web_search_tool_unless_disabled() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({
            "session": {"tools": [{"type": "function", "name": "web_search"}], "tool_choice": "auto"}
        })))
        .respond_with(ok_upstream())
        .expect(1)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::OK);

    let up2 = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up2)
        .await;
    let r = router(config(&up2.uri(), &[("LEXI_WEB_SEARCH", "false")]));
    send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    let body: serde_json::Value = up2.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert!(body["session"].get("tools").is_none(), "{body}");
    let (status, ..) = send(
        &r,
        search_req(r#"{"query":"x"}"#, Some(INSTALL), "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn search_runs_the_responses_web_search_tool_and_returns_plain_text() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", format!("Bearer {API_KEY}").as_str()))
        .and(body_partial_json(serde_json::json!({
            "model": "gpt-4.1-mini",
            "tools": [{"type": "web_search"}],
            "input": "weather in Paris"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": [
                {"type": "web_search_call", "id": "ws_1"},
                {"type": "message", "content": [{"type": "output_text",
                  "text": "**Sunny**, 12 degrees ([apnews.com](https://apnews.com/x?utm_source=openai))."}]}
            ]
        })))
        .expect(1)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, _, text) = send(
        &r,
        search_req(r#"{"query":"weather in Paris"}"#, Some(INSTALL), "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["text"], "Sunny, 12 degrees (apnews.com).");
}

#[tokio::test]
async fn search_validates_requests_and_checks_the_install_header() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let long = format!(r#"{{"query":"{}"}}"#, "a".repeat(301));
    for (body, install) in [
        ("{}", Some(INSTALL)),
        (r#"{"query":"  "}"#, Some(INSTALL)),
        (long.as_str(), Some(INSTALL)),
        (r#"{"query":"x","extra":1}"#, Some(INSTALL)),
        (r#"{"query":"x"}"#, None),
        (r#"{"query":"x"}"#, Some("short")),
    ] {
        let (status, ..) = send(&r, search_req(body, install, "10.0.0.1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn search_needs_attestation_when_configured_and_has_its_own_rate_limit() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}]
        })))
        .mount(&up)
        .await;
    let r = router(config(
        &up.uri(),
        &[
            ("LEXI_ATTEST_MODE", "shared-key"),
            ("LEXI_ATTEST_SHARED_KEY", "k"),
        ],
    ));
    let (status, ..) = send(
        &r,
        search_req(r#"{"query":"x"}"#, Some(INSTALL), "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let r = router(config(&up.uri(), &[("LEXI_RATE_SEARCH_PER_INSTALL", "2")]));
    for expect in [
        StatusCode::OK,
        StatusCode::OK,
        StatusCode::TOO_MANY_REQUESTS,
    ] {
        let (status, ..) = send(
            &r,
            search_req(r#"{"query":"x"}"#, Some(INSTALL), "10.0.0.1"),
        )
        .await;
        assert_eq!(status, expect);
    }
    // Calls to /v1/call-token are not affected by the search budget.
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_ne!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn instructions_carry_the_voices_gender() {
    for (voice, must, mustnot) in [
        (
            "cedar",
            "always speak about yourself in the masculine",
            "in the feminine",
        ),
        (
            "marin",
            "always speak about yourself in the feminine",
            "in the masculine",
        ),
        (
            "alloy",
            "always speak about yourself in the feminine",
            "in the masculine",
        ),
    ] {
        let up = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok_upstream())
            .mount(&up)
            .await;
        let r = router(config(&up.uri(), &[]));
        let body = format!(r#"{{"voice":"{voice}"}}"#);
        let (status, ..) = send(&r, post_body(&body, Some(INSTALL), None, "10.0.0.1")).await;
        assert_eq!(status, StatusCode::OK);
        let sent: serde_json::Value = up.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        let ins = sent["session"]["instructions"].as_str().unwrap();
        assert!(
            ins.contains(must) && !ins.contains(mustnot),
            "{voice}: {ins}"
        );
        assert!(ins.contains("Your name is Lexi"), "base prompt kept");
    }
}

fn gemini_ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({"name": "auth_tokens/abc123"}))
}

fn gemini_config(upstream: &str, extra: &[(&str, &str)]) -> Config {
    let mut e = vec![
        ("GOOGLE_API_KEY", "g-test-key"),
        ("LEXI_GEMINI_BASE_URL", upstream),
    ];
    e.extend_from_slice(extra);
    config(upstream, &e)
}

#[tokio::test]
async fn google_provider_mints_a_locked_ephemeral_token() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/auth_tokens"))
        .and(header("x-goog-api-key", "g-test-key"))
        .respond_with(gemini_ok())
        .expect(1)
        .mount(&up)
        .await;
    let r = router(gemini_config(&up.uri(), &[]));
    let (status, _, text) = send(
        &r,
        post_body(
            r#"{"provider":"google","voice":"kore"}"#,
            Some(INSTALL),
            None,
            "10.0.0.1",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["token"], "auth_tokens/abc123");
    assert_eq!(v["provider"], "google");
    assert_eq!(v["model"], "gemini-3.8-live");

    let sent: serde_json::Value = up.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(sent["uses"], 1);
    let setup = &sent["bidiGenerateContentSetup"];
    assert_eq!(setup["model"], "models/gemini-3.8-live");
    assert_eq!(
        setup["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
            ["voiceName"],
        "Kore"
    );
    assert_eq!(setup["generationConfig"]["responseModalities"][0], "AUDIO");
    let ins = setup["systemInstruction"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        ins.contains("Your name is Lexi") && ins.contains("in the feminine"),
        "{ins}"
    );
    // Default: Google's own search inside the Live session.
    assert!(setup["tools"][0]["googleSearch"].is_object(), "{setup}");
    // expireTime / newSessionExpireTime are RFC 3339 UTC timestamps.
    assert!(sent["newSessionExpireTime"]
        .as_str()
        .unwrap()
        .ends_with('Z'));
}

#[tokio::test]
async fn google_provider_uses_the_male_line_for_male_voices_and_validates() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(gemini_ok())
        .mount(&up)
        .await;
    let r = router(gemini_config(&up.uri(), &[]));
    let (status, ..) = send(
        &r,
        post_body(r#"{"provider":"google"}"#, Some(INSTALL), None, "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sent: serde_json::Value = up.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    let ins = sent["bidiGenerateContentSetup"]["systemInstruction"]["parts"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        ins.contains("in the masculine"),
        "default voice charon is male"
    );
    assert_eq!(
        sent["bidiGenerateContentSetup"]["generationConfig"]["speechConfig"]["voiceConfig"]
            ["prebuiltVoiceConfig"]["voiceName"],
        "Charon"
    );
    for body in [
        r#"{"provider":"google","voice":"cedar"}"#, // an OpenAI voice
        r#"{"provider":"google","model":"gpt-realtime-2.1"}"#, // models are OpenAI-only
        r#"{"provider":"azure"}"#,
        r#"{"voice":"kore"}"#, // Gemini voice without the provider
    ] {
        let (status, ..) = send(&r, post_body(body, Some(INSTALL), None, "10.0.0.2")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn google_provider_needs_its_own_key_and_maps_upstream_errors() {
    let up = MockServer::start().await;
    // No GOOGLE_API_KEY: google is not ready, openai still works.
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up)
        .await;
    let r = router(config(&up.uri(), &[]));
    let (status, ..) = send(
        &r,
        post_body(r#"{"provider":"google"}"#, Some(INSTALL), None, "10.0.0.1"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_IMPLEMENTED,
        "google has no key here"
    );
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.1")).await;
    assert_eq!(status, StatusCode::OK, "openai still works");

    let g = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(402))
        .mount(&g)
        .await;
    let r = router(gemini_config(&g.uri(), &[]));
    let (status, ..) = send(
        &r,
        post_body(r#"{"provider":"google"}"#, Some(INSTALL), None, "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn gemini_search_is_googles_builtin_tool_and_can_be_turned_off() {
    for (mode, on) in [("builtin", true), ("off", false)] {
        let up = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(gemini_ok())
            .mount(&up)
            .await;
        let r = router(gemini_config(&up.uri(), &[("LEXI_GEMINI_SEARCH", mode)]));
        let (status, ..) = send(
            &r,
            post_body(r#"{"provider":"google"}"#, Some(INSTALL), None, "10.0.0.1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let sent: serde_json::Value = up.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        let tools = &sent["bidiGenerateContentSetup"]["tools"];
        if on {
            assert!(tools[0]["googleSearch"].is_object());
            assert!(tools[0].get("functionDeclarations").is_none());
        } else {
            assert!(tools.is_null(), "{tools}");
        }
    }
}

#[tokio::test]
async fn search_is_openai_only_even_when_a_google_key_is_configured() {
    // Compartmentalized providers: /v1/search never touches Google, whatever keys are set.
    let o = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "from openai"}]}]
        })))
        .expect(1)
        .mount(&o)
        .await;
    let g = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&g)
        .await;
    let r = router(gemini_config(
        &g.uri(),
        &[("LEXI_OPENAI_BASE_URL", &o.uri())],
    ));
    let (status, _, text) = send(
        &r,
        search_req(r#"{"query":"x"}"#, Some(INSTALL), "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("from openai"));
}

#[tokio::test]
async fn google_works_without_an_openai_key_and_openai_without_a_google_key() {
    let g = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(gemini_ok())
        .mount(&g)
        .await;
    let uri = g.uri();
    let r = router(config(
        &uri,
        &[
            ("OPENAI_API_KEY", ""),
            ("GOOGLE_API_KEY", "g-test-key"),
            ("LEXI_GEMINI_BASE_URL", &uri),
        ],
    ));
    let (status, ..) = send(
        &r,
        post_body(r#"{"provider":"google"}"#, Some(INSTALL), None, "10.0.0.1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, ..) = send(&r, post(Some(INSTALL), None, "10.0.0.2")).await;
    assert_eq!(
        status,
        StatusCode::NOT_IMPLEMENTED,
        "openai has no key here, google has"
    );
    // The reverse is covered by google_provider_needs_its_own_key_and_maps_upstream_errors.
}

#[tokio::test]
async fn several_attestation_keys_are_accepted_one_per_friend() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .mount(&up)
        .await;
    let r = router(config(
        &up.uri(),
        &[
            ("LEXI_ATTEST_MODE", "shared-key"),
            ("LEXI_ATTEST_SHARED_KEY", "alice, bob ,carol"),
        ],
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (who, ip, expect) in [
        ("alice", "10.0.0.1", StatusCode::OK),
        ("bob", "10.0.0.2", StatusCode::OK),
        ("carol", "10.0.0.3", StatusCode::OK),
        ("mallory", "10.0.0.4", StatusCode::UNAUTHORIZED),
    ] {
        let h = sign(&Secret::new(who), now).unwrap();
        let (s, ..) = send(&r, post(Some(INSTALL), Some(&h), ip)).await;
        assert_eq!(s, expect, "{who}");
    }
}

#[tokio::test]
async fn a_wrong_phone_clock_is_reported_only_to_holders_of_a_valid_key() {
    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .expect(0)
        .mount(&up)
        .await;
    let r = router(config(
        &up.uri(),
        &[
            ("LEXI_ATTEST_MODE", "shared-key"),
            ("LEXI_ATTEST_SHARED_KEY", "k"),
        ],
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Right key, clock 10 minutes ahead: 412 "clock skew" (the app tells the user to fix the time).
    let h = sign(&Secret::new("k"), now + 600).unwrap();
    let (s, _, body) = send(&r, post(Some(INSTALL), Some(&h), "10.0.0.1")).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    assert!(body.contains("clock skew"));
    // Wrong key with the same bad clock: plain 401, so the clock hint is no oracle for guessing keys.
    let h = sign(&Secret::new("wrong"), now + 600).unwrap();
    let (s, ..) = send(&r, post(Some(INSTALL), Some(&h), "10.0.0.2")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
