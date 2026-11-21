#![allow(clippy::unwrap_used)]
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lexi_minter::attest::sign;
use lexi_minter::config::{Config, Secret};
use lexi_minter::{app, AppState};
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
    let mut b = Request::post("/v1/call-token");
    if let Some(i) = install {
        b = b.header("x-lexi-install", i);
    }
    if let Some(a) = attest {
        b = b.header("x-lexi-attest", a);
    }
    let mut req = b.body(Body::from("{}")).unwrap();
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
                        "audio": {"output": {"voice": "marin"}}}
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

    // No API key: alive but not ready, and minting is 503.
    let mut cfg = config(&up.uri(), &[]);
    cfg.api_key = None;
    let r = router(cfg);
    assert_eq!(send(&r, get("/healthz")).await.0, StatusCode::OK);
    assert_eq!(
        send(&r, get("/readyz")).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        send(&r, post(Some(INSTALL), None, "10.0.0.1")).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
