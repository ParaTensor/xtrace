use axum::Router;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::{
    app::build_router,
    http::metrics::{metrics_worker, MetricsBatchRequest},
    ingest::batch::{ingest_worker, BatchIngestRequest},
    state::{AppState, IngestStats, RateLimitStats},
};

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

    let (ingest_tx, ingest_rx) = mpsc::channel::<BatchIngestRequest>(1000);
    let (metrics_tx, metrics_rx) = mpsc::channel::<MetricsBatchRequest>(5000);
    let ingest_stats = Arc::new(IngestStats::new());
    let default_project_id: Arc<str> = Arc::from("default");

    let db_conn = crate::state::DatabaseConnection::Postgres(pool);

    tokio::spawn(ingest_worker(
        db_conn.clone(),
        default_project_id.clone(),
        ingest_rx,
        ingest_stats.clone(),
    ));
    tokio::spawn(metrics_worker(
        db_conn.clone(),
        default_project_id.clone(),
        metrics_rx,
    ));

    let state = AppState {
        db: db_conn,
        api_bearer_token: Arc::from(bearer_token),
        langfuse_public_key: None,
        langfuse_secret_key: None,
        default_project_id,
        ingest_tx,
        metrics_tx,
        query_limiter: AppState::build_limiter(100, 200),
        rate_limit_stats: Arc::new(RateLimitStats::new()),
        ingest_stats,
        rate_limit_qps: 100,
        rate_limit_burst: 200,
        allow_unauthenticated_compat: false,
    };

    Ok(build_router(state, 20 * 1024 * 1024))
}
