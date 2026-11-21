//! lexi-minter: issues one short-lived OpenAI ephemeral key per call (SPEC §5).

pub mod attest;
pub mod config;
pub mod openai;
pub mod ratelimit;

use attest::Attestor;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use config::Config;
use openai::OpenAiClient;
use ratelimit::Limiter;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
const INSTALL_HEADER: &str = "x-lexi-install";
const ATTEST_HEADER: &str = "x-lexi-attest";

#[derive(Debug)]
pub struct AppState {
    pub cfg: Config,
    pub openai: OpenAiClient,
    pub attestor: Attestor,
    pub per_install: Limiter,
    pub per_ip: Limiter,
}

impl AppState {
    pub fn new(cfg: Config) -> Result<Arc<Self>, String> {
        let openai = OpenAiClient::new(&cfg).map_err(|e| format!("http client: {e}"))?;
        let attestor = Attestor::from_config(&cfg)?;
        Ok(Arc::new(Self {
            per_install: Limiter::per_hour(cfg.rate_per_install),
            per_ip: Limiter::per_hour(cfg.rate_per_ip),
            openai,
            attestor,
            cfg,
        }))
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TokenRequest {
    #[allow(dead_code)] // accepted for forward compatibility; v1 is English only
    locale: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    token: String,
    expires_at: i64,
    model: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

fn err(status: StatusCode, msg: &'static str) -> Response {
    (status, Json(ErrorBody { error: msg })).into_response()
}

fn too_many(scope: &'static str, retry: std::time::Duration) -> Response {
    metrics::counter!("lexi_rate_limited_total", "scope" => scope).increment(1);
    let secs = retry.as_secs().max(1).to_string();
    let mut resp = err(StatusCode::TOO_MANY_REQUESTS, "rate limited");
    if let Ok(v) = header::HeaderValue::from_str(&secs) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

fn client_ip(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    if state.cfg.trust_forwarded_for {
        let fwd = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|s| s.trim().parse::<IpAddr>().ok());
        if let Some(ip) = fwd {
            return ip;
        }
    }
    peer.ip()
}

fn valid_install_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn call_token(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let started = Instant::now();
    let resp = call_token_inner(&state, peer, &headers, &body).await;
    let status = resp.status().as_u16().to_string();
    metrics::counter!("lexi_requests_total", "status" => status).increment(1);
    metrics::histogram!("lexi_request_duration_seconds").record(started.elapsed().as_secs_f64());
    resp
}

async fn call_token_inner(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    if !state.openai.has_key() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "not ready");
    }
    if !body.is_empty() && serde_json::from_slice::<TokenRequest>(body).is_err() {
        return err(StatusCode::BAD_REQUEST, "invalid body");
    }
    let install = headers
        .get(INSTALL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .filter(|s| valid_install_hash(s));
    let Some(install) = install else {
        return err(StatusCode::BAD_REQUEST, "missing or invalid X-Lexi-Install");
    };

    let ip = client_ip(state, headers, peer).to_string();
    if let Err(retry) = state.per_ip.check(&ip) {
        return too_many("ip", retry);
    }
    let attest = headers.get(ATTEST_HEADER).and_then(|v| v.to_str().ok());
    if state.attestor.verify(attest).is_err() {
        tracing::warn!("attestation failed");
        return err(StatusCode::UNAUTHORIZED, "attestation failed");
    }
    if let Err(retry) = state.per_install.check(&install) {
        return too_many("install", retry);
    }

    match state.openai.mint(&install).await {
        Ok(secret) => Json(TokenResponse {
            token: secret.value.expose().to_owned(),
            expires_at: secret.expires_at,
            model: state.cfg.model.clone(),
        })
        .into_response(),
        Err(e) => {
            metrics::counter!("lexi_upstream_errors_total").increment(1);
            // Status code only; never the body (it could echo secrets).
            tracing::error!(error = %e, "upstream mint failed");
            err(StatusCode::BAD_GATEWAY, "upstream error")
        }
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    if state.openai.has_key() {
        (StatusCode::OK, "ready").into_response()
    } else {
        err(StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

/// Public API router (port 8080).
pub fn app(state: Arc<AppState>) -> Router {
    let timeout = state.cfg.request_timeout;
    Router::new()
        .route("/v1/call-token", post(call_token))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
        .layer(TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
            let id = req
                .headers()
                .get(&X_REQUEST_ID)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-");
            // Method + path + request id only; never headers (Authorization, install ID).
            tracing::info_span!("request", method = %req.method(), path = %req.uri().path(), request_id = %id)
        }))
        .layer(PropagateRequestIdLayer::new(X_REQUEST_ID))
        .layer(SetRequestIdLayer::new(X_REQUEST_ID, MakeRequestUuid))
        .layer(TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, timeout))
        .layer(RequestBodyLimitLayer::new(4096))
}

/// Metrics router (port 9090).
pub fn metrics_app(handle: metrics_exporter_prometheus::PrometheusHandle) -> Router {
    Router::new().route("/metrics", get(move || async move { handle.render() }))
}
