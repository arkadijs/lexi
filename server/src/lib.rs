//! lexi: issues one short-lived OpenAI ephemeral key per call (SPEC §5).

pub mod attest;
pub mod config;
pub mod gemini;
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
    pub gemini: gemini::GeminiClient,
    pub attestor: Attestor,
    pub per_install: Limiter,
    pub per_ip: Limiter,
    pub per_search_install: Limiter,
    pub per_search_ip: Limiter,
}

impl AppState {
    pub fn new(cfg: Config) -> Result<Arc<Self>, String> {
        let openai = OpenAiClient::new(&cfg).map_err(|e| format!("http client: {e}"))?;
        let gemini = gemini::GeminiClient::new(&cfg).map_err(|e| format!("http client: {e}"))?;
        let attestor = Attestor::from_config(&cfg)?;
        Ok(Arc::new(Self {
            per_install: Limiter::per_hour(cfg.rate_per_install),
            per_ip: Limiter::per_hour(cfg.rate_per_ip),
            per_search_install: Limiter::per_hour(cfg.rate_search_per_install),
            per_search_ip: Limiter::per_hour(cfg.rate_search_per_ip),
            openai,
            gemini,
            attestor,
            cfg,
        }))
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TokenRequest {
    #[allow(dead_code)] // accepted for forward compatibility
    locale: Option<String>,
    /// One of the configured `LEXI_VOICES`; absent = the default voice.
    voice: Option<String>,
    /// One of the configured `LEXI_MODELS`; absent = the default model. Not accepted with `provider: google`
    /// (the Gemini model is fixed by `LEXI_GEMINI_MODEL`).
    model: Option<String>,
    /// `openai` (default) or `google`. The voice is validated against that provider's list.
    provider: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    token: String,
    expires_at: i64,
    model: String,
    provider: &'static str,
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

/// The checks every client request passes: install header, per-IP limit, attestation, per-install limit.
/// Returns the lower-cased install hash.
fn authorize(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    per_ip: &Limiter,
    per_install: &Limiter,
) -> Result<String, Box<Response>> {
    let install = headers
        .get(INSTALL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .filter(|s| valid_install_hash(s));
    let Some(install) = install else {
        return Err(Box::new(err(
            StatusCode::BAD_REQUEST,
            "missing or invalid X-Lexi-Install",
        )));
    };

    let ip = client_ip(state, headers, peer).to_string();
    if let Err(retry) = per_ip.check(&ip) {
        return Err(Box::new(too_many("ip", retry)));
    }
    let attest = headers.get(ATTEST_HEADER).and_then(|v| v.to_str().ok());
    match state.attestor.verify(attest) {
        Ok(()) => {}
        // 412: the key is right but the phone's clock is off by more than 5 minutes (the app says so).
        Err(attest::AttestError::Skew) => {
            tracing::warn!("attestation: client clock skew");
            return Err(Box::new(err(StatusCode::PRECONDITION_FAILED, "clock skew")));
        }
        Err(attest::AttestError::Invalid) => {
            tracing::warn!("attestation failed");
            return Err(Box::new(err(
                StatusCode::UNAUTHORIZED,
                "attestation failed",
            )));
        }
    }
    if let Err(retry) = per_install.check(&install) {
        return Err(Box::new(too_many("install", retry)));
    }
    Ok(install)
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
    let request: TokenRequest = if body.is_empty() {
        TokenRequest::default()
    } else {
        match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(_) => return err(StatusCode::BAD_REQUEST, "invalid body"),
        }
    };
    let google = match request.provider.as_deref() {
        None | Some("openai") => false,
        Some("google") => true,
        Some(_) => return err(StatusCode::BAD_REQUEST, "unknown provider"),
    };
    let ready = if google {
        state.gemini.has_key()
    } else {
        state.openai.has_key()
    };
    if !ready {
        return err(StatusCode::SERVICE_UNAVAILABLE, "not ready");
    }
    if let Some(v) = &request.voice {
        let known = if google {
            state
                .cfg
                .gemini_voices
                .iter()
                .any(|(allowed, _)| allowed == v)
        } else {
            state.cfg.voices.iter().any(|allowed| allowed == v)
        };
        if !known {
            return err(StatusCode::BAD_REQUEST, "unknown voice");
        }
    }
    if let Some(m) = &request.model {
        if google || !state.cfg.models.iter().any(|allowed| allowed == m) {
            return err(StatusCode::BAD_REQUEST, "unknown model");
        }
    }
    let install = match authorize(state, peer, headers, &state.per_ip, &state.per_install) {
        Ok(i) => i,
        Err(r) => return *r,
    };

    let minted = if google {
        let voice = request.voice.as_deref().unwrap_or(&state.cfg.gemini_voice);
        state
            .gemini
            .mint(voice)
            .await
            .map(|s| (s, state.gemini.model().to_owned(), "google"))
    } else {
        state
            .openai
            .mint(&install, request.voice.as_deref(), request.model.as_deref())
            .await
            .map(|s| {
                let model = request
                    .model
                    .clone()
                    .unwrap_or_else(|| state.cfg.model.clone());
                (s, model, "openai")
            })
    };
    match minted {
        Ok((secret, model, provider)) => Json(TokenResponse {
            token: secret.value.expose().to_owned(),
            expires_at: secret.expires_at,
            model,
            provider,
        })
        .into_response(),
        Err(e) => {
            metrics::counter!("lexi_upstream_errors_total").increment(1);
            // Status code only; never the body (it could echo secrets).
            tracing::error!(error = %e, provider = if google { "google" } else { "openai" }, "upstream mint failed");
            err(StatusCode::BAD_GATEWAY, "upstream error")
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRequest {
    query: String,
}

#[derive(Serialize)]
struct SearchResponse {
    text: String,
}

/// Longest query accepted (characters); the model's own queries are a few words.
const MAX_QUERY_CHARS: usize = 300;

/// `POST /v1/search`: the client's answer to the bot's `web_search` tool call.
async fn search(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let started = Instant::now();
    let resp = search_inner(&state, peer, &headers, &body).await;
    let status = resp.status().as_u16().to_string();
    metrics::counter!("lexi_search_requests_total", "status" => status).increment(1);
    metrics::histogram!("lexi_search_duration_seconds").record(started.elapsed().as_secs_f64());
    resp
}

async fn search_inner(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    if !state.cfg.web_search {
        return err(StatusCode::NOT_FOUND, "web search is off");
    }
    // OpenAI's own search (the Responses `web_search` tool): the OpenAI bot's tool. Gemini searches inside
    // its Live session and never comes here.
    if !state.openai.has_key() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "not ready");
    }
    let Ok(request) = serde_json::from_slice::<SearchRequest>(body) else {
        return err(StatusCode::BAD_REQUEST, "invalid body");
    };
    let query = request.query.trim();
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        return err(StatusCode::BAD_REQUEST, "invalid query");
    }
    let install = match authorize(
        state,
        peer,
        headers,
        &state.per_search_ip,
        &state.per_search_install,
    ) {
        Ok(i) => i,
        Err(r) => return *r,
    };
    // The query text is never logged (it is what the user said).
    match state.openai.search(&install, query).await {
        Ok(text) => Json(SearchResponse { text }).into_response(),
        Err(e) => {
            metrics::counter!("lexi_upstream_errors_total").increment(1);
            tracing::error!(error = %e, "upstream search failed");
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
        .route("/v1/search", post(search))
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
