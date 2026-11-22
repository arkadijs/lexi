//! Runs in its own test binary: tracing caches callsite interest process-wide, so sharing a
//! process with tests that have no subscriber would make this one flaky.
#![allow(clippy::unwrap_used)]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use lexi::attest::sign;
use lexi::config::{Config, Secret};
use lexi::{app, AppState};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use wiremock::matchers::method;
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

fn post(install: &str, attest: &str) -> Request<Body> {
    let mut req = Request::post("/v1/call-token")
        .header("x-lexi-install", install)
        .header("x-lexi-attest", attest)
        .body(Body::from("{}"))
        .unwrap();
    let addr: SocketAddr = "10.0.0.1:5555".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn send(r: &axum::Router, req: Request<Body>) {
    r.clone().oneshot(req).await.unwrap();
}

fn ok_upstream() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(
        serde_json::json!({"value": TOKEN, "expires_at": 1_900_000_000, "session": {}}),
    )
}

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[tokio::test]
async fn logs_never_contain_tokens_keys_or_install_ids() {
    let buf = Buf::default();
    let sub = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(buf.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(sub);

    let up = MockServer::start().await;
    // success, 4xx, 5xx, attestation failure: exercise every logging path
    Mock::given(method("POST"))
        .respond_with(ok_upstream())
        .up_to_n_times(1)
        .mount(&up)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string(format!("echo {API_KEY} {TOKEN}")))
        .mount(&up)
        .await;
    let r = router(config(
        &up.uri(),
        &[
            ("LEXI_ATTEST_MODE", "shared-key"),
            ("LEXI_ATTEST_SHARED_KEY", "sharedsecret"),
        ],
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let good = sign(&Secret::new("sharedsecret"), now).unwrap();
    send(&r, post(INSTALL, &good)).await;
    send(&r, post(INSTALL, &good)).await;
    send(&r, post(INSTALL, "bogus")).await;

    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.is_empty(), "expected some log output");
    for secret in [API_KEY, TOKEN, INSTALL, "sharedsecret", good.as_str()] {
        assert!(!logs.contains(secret), "log leaked {secret}");
    }
}
