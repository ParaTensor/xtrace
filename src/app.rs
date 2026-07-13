use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Method},
    middleware::{self},
    routing::{get, post, put},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};

use crate::http::common::{healthz, readyz};
use crate::http::{
    auth::{auth, rate_limit},
    auth_context::AuthRegistry,
    media,
    metrics::{self, metrics_worker, post_metrics_batch, MetricsBatchRequest},
    ops::{
        get_ingest_stats, get_metrics_label_governance_stats, get_rate_limit_stats,
        get_retention_stats,
    },
    projects::get_projects,
    traces,
};
use crate::ingest::batch::{ingest_worker, post_batch, IngestEnvelope};
use crate::ingest::otlp;
use crate::metrics::label_governance::LabelGovernance;
use crate::retention::retention_worker;
use crate::state::{AppState, IngestStats, RateLimitStats, ServerConfig};

/// Browser CORS for local dashboard / cross-origin clients.
/// `XTRACE_CORS_ORIGINS`: comma-separated origins, or `*` for any (dev only).
fn cors_layer() -> CorsLayer {
    let raw = std::env::var("XTRACE_CORS_ORIGINS").unwrap_or_else(|_| {
        "http://127.0.0.1:5173,http://localhost:5173".to_string()
    });
    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
        Method::HEAD,
    ];
    let headers = [
        header::AUTHORIZATION,
        header::CONTENT_TYPE,
        header::ACCEPT,
    ];

    if raw.trim() == "*" {
        return CorsLayer::new()
            .allow_origin(AllowOrigin::any())
            .allow_methods(methods)
            .allow_headers(headers)
            .max_age(Duration::from_secs(600));
    }

    let origins: Vec<HeaderValue> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(methods)
        .allow_headers(headers)
        .max_age(Duration::from_secs(600))
}

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
        .route("/api/public/otel/v1/traces", post(otlp::post_otel_traces))
        .route("/api/public/otel/v1/metrics", post(otlp::post_otel_metrics))
        .route("/api/public/media", post(media::post_media))
        .route(
            "/api/public/media/:mediaId",
            get(media::get_media).patch(media::patch_media),
        )
        .route(
            "/api/public/media/:mediaId/upload",
            put(media::put_media_upload),
        );

    let protected_routes = Router::new()
        .merge(query_routes)
        .merge(write_routes)
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));

    let media_content_routes = Router::new()
        .route(
            "/api/public/media/:mediaId/content",
            get(media::get_media_content),
        )
        .with_state(state.clone());

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/internal/rate_limit_stats", get(get_rate_limit_stats))
        .route("/api/internal/ingest_stats", get(get_ingest_stats))
        .route(
            "/api/internal/metrics_label_governance_stats",
            get(get_metrics_label_governance_stats),
        )
        .route("/api/internal/retention_stats", get(get_retention_stats))
        .merge(media_content_routes)
        .merge(protected_routes)
        .layer(DefaultBodyLimit::max(max_body))
        .with_state(state)
        .layer(cors_layer())
        .layer(TraceLayer::new_for_http())
}

/// Start xtrace server (blocks until shutdown signal)
pub async fn run_server(config: ServerConfig) -> anyhow::Result<()> {
    let mock_storage = std::env::var("XTRACE_MOCK_STORAGE")
        .map(|s| s == "true")
        .unwrap_or(false);
    let json_dir_env = std::env::var("XTRACE_JSON_DIR").ok();

    let db_conn = if mock_storage || json_dir_env.is_some() {
        let dir = json_dir_env
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("./.xtrace_data"));
        let mem_db = Arc::new(crate::state::MemoryDb::new(Some(dir)));
        mem_db.clone().spawn_sync_loop();
        crate::state::DatabaseConnection::Memory(mem_db)
    } else {
        let pool = PgPoolOptions::new()
            .max_connections(20)
            .connect(&config.database_url)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        crate::state::DatabaseConnection::Postgres(pool)
    };

    let (ingest_tx, ingest_rx) = mpsc::channel::<IngestEnvelope>(1000);
    let (metrics_tx, metrics_rx) = mpsc::channel::<MetricsBatchRequest>(5000);

    let qps = config.rate_limit_qps;
    let burst = config.rate_limit_burst;
    let query_limiter = AppState::build_limiter(qps, burst);
    let rate_limit_stats = Arc::new(RateLimitStats::new());
    let ingest_stats = Arc::new(IngestStats::new());
    let retention_stats = Arc::new(crate::retention::RetentionStats::default());
    let auth_registry = AuthRegistry::from_config(
        config.default_project_id.clone(),
        config.api_bearer_token.clone(),
        config.api_read_bearer_token.clone(),
        config.langfuse_public_key.clone(),
        config.langfuse_secret_key.clone(),
        config.project_tokens.as_deref(),
        config.project_basic_auth.as_deref(),
    );

    std::fs::create_dir_all(&config.media_dir)?;

    let state = AppState {
        db: db_conn.clone(),
        api_bearer_token: Arc::from(config.api_bearer_token),
        langfuse_public_key: config.langfuse_public_key.map(Arc::from),
        langfuse_secret_key: config.langfuse_secret_key.map(Arc::from),
        default_project_id: auth_registry.default_project_id.clone(),
        auth_registry,
        ingest_tx,
        metrics_tx,
        query_limiter,
        rate_limit_stats,
        ingest_stats: ingest_stats.clone(),
        rate_limit_qps: qps,
        rate_limit_burst: burst,
        allow_unauthenticated_compat: config.allow_unauthenticated_compat,
        media_dir: Arc::from(config.media_dir),
        public_base_url: config.public_base_url.map(Arc::from),
        media_max_content_length: config.media_max_content_length,
        label_governance: LabelGovernance::new(config.label_governance),
        metrics_query_max_series: config.metrics_query_max_series,
        metrics_query_max_points_per_series: config.metrics_query_max_points_per_series,
        retention_stats: retention_stats.clone(),
    };

    tokio::spawn(ingest_worker(state.db.clone(), ingest_rx, ingest_stats));

    tokio::spawn(metrics_worker(state.db.clone(), metrics_rx));

    if config.retention.enabled() {
        tokio::spawn(retention_worker(
            state.db.clone(),
            config.retention.clone(),
            retention_stats,
        ));
    }

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
