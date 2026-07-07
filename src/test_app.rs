use axum::Router;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::{
    app::build_router,
    http::auth_context::AuthRegistry,
    http::metrics::{metrics_worker, MetricsBatchRequest},
    ingest::batch::{ingest_worker, IngestEnvelope},
    metrics::label_governance::LabelGovernance,
    retention::RetentionStats,
    state::{AppState, IngestStats, RateLimitStats},
};

fn build_test_state(
    db_conn: crate::state::DatabaseConnection,
    bearer_token: &str,
    read_token: Option<&str>,
) -> AppState {
    let (ingest_tx, ingest_rx) = mpsc::channel::<IngestEnvelope>(1000);
    let (metrics_tx, metrics_rx) = mpsc::channel::<MetricsBatchRequest>(5000);
    let ingest_stats = Arc::new(IngestStats::new());
    let auth_registry = AuthRegistry::from_config(
        "default".to_string(),
        bearer_token.to_string(),
        read_token.map(str::to_string),
        None,
        None,
        None,
        None,
    );

    tokio::spawn(ingest_worker(
        db_conn.clone(),
        ingest_rx,
        ingest_stats.clone(),
    ));
    tokio::spawn(metrics_worker(db_conn.clone(), metrics_rx));

    let media_dir = std::path::PathBuf::from(".xtrace_test_media");
    let _ = std::fs::create_dir_all(&media_dir);

    AppState {
        db: db_conn,
        api_bearer_token: Arc::from(bearer_token),
        langfuse_public_key: None,
        langfuse_secret_key: None,
        default_project_id: auth_registry.default_project_id.clone(),
        auth_registry,
        ingest_tx,
        metrics_tx,
        query_limiter: AppState::build_limiter(100, 200),
        rate_limit_stats: Arc::new(RateLimitStats::new()),
        ingest_stats,
        rate_limit_qps: 100,
        rate_limit_burst: 200,
        allow_unauthenticated_compat: false,
        media_dir: Arc::from(media_dir),
        public_base_url: Some(Arc::from("http://127.0.0.1:8742")),
        media_max_content_length: 20 * 1024 * 1024,
        label_governance: LabelGovernance::default(),
        metrics_query_max_series: 50,
        metrics_query_max_points_per_series: 1000,
        retention_stats: Arc::new(RetentionStats::default()),
    }
}

/// Build a router wired to PostgreSQL for integration tests.
#[doc(hidden)]
pub async fn setup_test_router(
    database_url: &str,
    bearer_token: &str,
) -> Result<Router, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    let db_conn = crate::state::DatabaseConnection::Postgres(pool);
    let state = build_test_state(db_conn, bearer_token, None);
    Ok(build_router(state, 20 * 1024 * 1024))
}

/// Build an in-memory mock router with multi-tenant project token map.
#[doc(hidden)]
pub async fn setup_mock_router_with_project_tokens(project_tokens: &str) -> Router {
    let mem_db = Arc::new(crate::state::MemoryDb::new(None));
    let db_conn = crate::state::DatabaseConnection::Memory(mem_db);
    let (ingest_tx, ingest_rx) = mpsc::channel::<IngestEnvelope>(1000);
    let (metrics_tx, metrics_rx) = mpsc::channel::<MetricsBatchRequest>(5000);
    let ingest_stats = Arc::new(IngestStats::new());
    let auth_registry = AuthRegistry::from_config(
        "default".to_string(),
        "unused".to_string(),
        None,
        None,
        None,
        Some(project_tokens),
        None,
    );

    tokio::spawn(ingest_worker(
        db_conn.clone(),
        ingest_rx,
        ingest_stats.clone(),
    ));
    tokio::spawn(metrics_worker(db_conn.clone(), metrics_rx));

    let media_dir = std::path::PathBuf::from(".xtrace_test_media");
    let _ = std::fs::create_dir_all(&media_dir);

    let state = AppState {
        db: db_conn,
        api_bearer_token: Arc::from("unused"),
        langfuse_public_key: None,
        langfuse_secret_key: None,
        default_project_id: auth_registry.default_project_id.clone(),
        auth_registry,
        ingest_tx,
        metrics_tx,
        query_limiter: AppState::build_limiter(100, 200),
        rate_limit_stats: Arc::new(RateLimitStats::new()),
        ingest_stats,
        rate_limit_qps: 100,
        rate_limit_burst: 200,
        allow_unauthenticated_compat: false,
        media_dir: Arc::from(media_dir),
        public_base_url: Some(Arc::from("http://127.0.0.1:8742")),
        media_max_content_length: 20 * 1024 * 1024,
        label_governance: LabelGovernance::default(),
        metrics_query_max_series: 50,
        metrics_query_max_points_per_series: 1000,
        retention_stats: Arc::new(RetentionStats::default()),
    };

    build_router(state, 20 * 1024 * 1024)
}

/// Build an in-memory mock router with optional read-only token.
#[doc(hidden)]
pub async fn setup_mock_router_with_tokens(write_token: &str, read_token: Option<&str>) -> Router {
    let mem_db = Arc::new(crate::state::MemoryDb::new(None));
    let db_conn = crate::state::DatabaseConnection::Memory(mem_db);
    let state = build_test_state(db_conn, write_token, read_token);
    build_router(state, 20 * 1024 * 1024)
}

/// Build an in-memory mock router for integration tests.
#[doc(hidden)]
pub async fn setup_mock_router(bearer_token: &str) -> Router {
    setup_mock_router_with_tokens(bearer_token, None).await
}
