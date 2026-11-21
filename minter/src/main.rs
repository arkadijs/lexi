use lexi_minter::{app, config::Config, metrics_app, AppState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

const DRAIN: Duration = Duration::from_secs(10);

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hyper=warn".into()),
        )
        .init();

    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "invalid configuration");
            return std::process::ExitCode::FAILURE;
        }
    };
    if cfg.api_key.is_none() {
        tracing::warn!("OPENAI_API_KEY is not set; /readyz will report not ready");
    }
    let (bind, metrics_bind) = (cfg.bind, cfg.metrics_bind);
    let state = match AppState::new(cfg) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "startup failed");
            return std::process::ExitCode::FAILURE;
        }
    };

    let handle = match metrics_exporter_prometheus::PrometheusBuilder::new()
        .set_buckets(&[0.025, 0.05, 0.1, 0.2, 0.4, 0.8, 1.6, 3.2, 6.4])
        .and_then(|b| b.install_recorder())
    {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "metrics init failed");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Bound limiter memory.
    let gc_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            gc_state.per_ip.gc();
            gc_state.per_install.gc();
        }
    });

    let (api_l, met_l) = match (
        tokio::net::TcpListener::bind(bind).await,
        tokio::net::TcpListener::bind(metrics_bind).await,
    ) {
        (Ok(a), Ok(m)) => (a, m),
        (a, m) => {
            tracing::error!(api = ?a.err(), metrics = ?m.err(), "bind failed");
            return std::process::ExitCode::FAILURE;
        }
    };
    tracing::info!(%bind, %metrics_bind, "listening");

    let stop = Arc::new(Notify::new());
    let wait = |n: Arc<Notify>| async move { n.notified().await };
    let api = axum::serve(
        api_l,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(wait(Arc::clone(&stop)));
    let met =
        axum::serve(met_l, metrics_app(handle)).with_graceful_shutdown(wait(Arc::clone(&stop)));
    let servers = tokio::spawn(async move {
        let _ = tokio::join!(api, met);
    });

    shutdown_signal().await;
    tracing::info!("shutdown requested; draining");
    stop.notify_waiters();
    // `notify_waiters` only wakes tasks already waiting; they are, since serve was spawned earlier.
    if tokio::time::timeout(DRAIN, servers).await.is_err() {
        tracing::warn!("drain timed out; exiting");
    }
    std::process::ExitCode::SUCCESS
}
