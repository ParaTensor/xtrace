use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;
use xtrace::test_app::setup_test_router;

async fn setup_app() -> (axum::Router, String) {
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    let token = "integration-test-token".to_string();
    let router = setup_test_router(&database_url, &token)
        .await
        .expect("setup test router");
    (router, token)
}

fn authed_request(method: &str, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(json) = body {
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json.to_string()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    }
}

#[tokio::test]
async fn healthz_is_unauthenticated() {
    let (app, _) = setup_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn protected_route_requires_auth() {
    let (app, _) = setup_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/public/traces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ingest_and_read_trace_round_trip() {
    let (app, token) = setup_app().await;
    let trace_id = Uuid::new_v4();
    let obs_id = Uuid::new_v4();

    let ingest = authed_request(
        "POST",
        "/v1/l/batch",
        &token,
        Some(json!({
            "trace": {
                "id": trace_id,
                "timestamp": Utc::now(),
                "name": "integration-test",
                "userId": "alice",
                "tags": ["test"]
            },
            "observations": [{
                "id": obs_id,
                "traceId": trace_id,
                "type": "GENERATION",
                "name": "llm",
                "startTime": Utc::now(),
                "endTime": Utc::now(),
                "model": "gpt-4o-mini",
                "input": {"role": "user", "content": "hi"},
                "output": {"role": "assistant", "content": "hello"}
            }]
        })),
    );

    let ingest_response = app.clone().oneshot(ingest).await.unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let list_response = app
        .clone()
        .oneshot(authed_request(
            "GET",
            "/api/public/traces?page=1&limit=10",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);

    let detail_response = app
        .oneshot(authed_request(
            "GET",
            &format!("/api/public/traces/{trace_id}"),
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(detail_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn trace_detail_is_scoped_to_default_project() {
    let (app, token) = setup_app().await;
    let trace_id = Uuid::new_v4();

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();

    sqlx::query(
        r#"
INSERT INTO traces (
  id, project_id, environment, timestamp, name, tags, public, bookmarked, created_at, updated_at
) VALUES ($1, 'other-project', 'default', NOW(), 'foreign', '{}', false, false, NOW(), NOW())
        "#,
    )
    .bind(trace_id)
    .execute(&pool)
    .await
    .expect("insert foreign trace");

    let response = app
        .oneshot(authed_request(
            "GET",
            &format!("/api/public/traces/{trace_id}"),
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metrics_query_returns_series() {
    let (app, token) = setup_app().await;

    let write_response = app
        .clone()
        .oneshot(authed_request(
            "POST",
            "/v1/metrics/batch",
            &token,
            Some(json!({
                "metrics": [{
                    "name": "gpu_utilization",
                    "labels": {"node_id": "node-1"},
                    "value": 42.0,
                    "timestamp": Utc::now()
                }]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(write_response.status(), StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let query_response = app
        .oneshot(authed_request(
            "GET",
            "/api/public/metrics/query?name=gpu_utilization&step=1m&agg=avg",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(query_response.status(), StatusCode::OK);
}
