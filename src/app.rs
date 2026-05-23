use axum::{
    extract::DefaultBodyLimit,
    middleware::{self},
    routing::{get, post},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::mpsc;
use tower_http::trace::TraceLayer;

use crate::http::common::{healthz, readyz};
use crate::http::{
    auth::{auth, rate_limit},
    metrics::{self, metrics_worker, post_metrics_batch, MetricsBatchRequest},
    ops::{get_ingest_stats, get_rate_limit_stats},
    projects::get_projects,
    traces,
};
use crate::ingest::batch::{ingest_worker, post_batch, BatchIngestRequest};
use crate::ingest::otlp;
use crate::state::{AppState, IngestStats, RateLimitStats, ServerConfig};

/// Build the Axum router (used by the server and integration tests).
pub fn build_router(state: AppState, max_body: usize) -> Router {
    let query_routes = Router::new()
        .route("/api/public/metrics", get(metrics::get_metrics_overview))
        .route("/api/public/metrics/daily", get(metrics::get_metrics_daily))
        .route("/api/public/metrics/query", get(metrics::get_metrics_query))
        .route("/api/public/metrics/names", get(metrics::get_metrics_names))
        .route("/api/public/traces", get(traces::get_traces))
        .route("/api/public/traces/:traceId", get(traces::get_trace))
        .route_layer(middleware::from_fn_with_state(state.clone(), rate_limit));

    let write_routes = Router::new()
        .route("/v1/l/batch", post(post_batch))
        .route("/v1/metrics/batch", post(post_metrics_batch))
        .route("/api/public/projects", get(get_projects))
        .route("/api/public/otel/v1/traces", post(otlp::post_otel_traces));

    let protected_routes = Router::new()
        .merge(query_routes)
        .merge(write_routes)
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/internal/rate_limit_stats", get(get_rate_limit_stats))
        .route("/api/internal/ingest_stats", get(get_ingest_stats))
        .merge(protected_routes)
        .layer(DefaultBodyLimit::max(max_body))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Start xtrace server (blocks until shutdown signal)
pub async fn run_server(config: ServerConfig) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&config.database_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    let (ingest_tx, ingest_rx) = mpsc::channel::<BatchIngestRequest>(1000);
    let (metrics_tx, metrics_rx) = mpsc::channel::<MetricsBatchRequest>(5000);

    let qps = config.rate_limit_qps;
    let burst = config.rate_limit_burst;
    let query_limiter = AppState::build_limiter(qps, burst);
    let rate_limit_stats = Arc::new(RateLimitStats::new());
    let ingest_stats = Arc::new(IngestStats::new());

    let state = AppState {
        pool,
        api_bearer_token: Arc::from(config.api_bearer_token),
        langfuse_public_key: config.langfuse_public_key.map(Arc::from),
        langfuse_secret_key: config.langfuse_secret_key.map(Arc::from),
        default_project_id: Arc::from(config.default_project_id),
        ingest_tx,
        metrics_tx,
        query_limiter,
        rate_limit_stats,
        ingest_stats: ingest_stats.clone(),
        rate_limit_qps: qps,
        rate_limit_burst: burst,
        allow_unauthenticated_compat: config.allow_unauthenticated_compat,
    };

    tokio::spawn(ingest_worker(
        state.pool.clone(),
        state.default_project_id.clone(),
        ingest_rx,
        ingest_stats,
    ));

    tokio::spawn(metrics_worker(
        state.pool.clone(),
        state.default_project_id.clone(),
        metrics_rx,
    ));

    let addr: SocketAddr = config.bind_addr.parse()?;
    tracing::info!(
        "listening on {} (rate_limit: {} qps, burst {})",
        addr,
        qps,
        burst
    );

    let max_body = config.max_request_body_bytes;
    let app = build_router(state, max_body);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
