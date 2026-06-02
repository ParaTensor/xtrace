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

fn url_encode_json(val: &Value) -> String {
    let s = val.to_string();
    s.replace('{', "%7B")
        .replace('}', "%7D")
        .replace('"', "%22")
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace(':', "%3A")
        .replace(',', "%2C")
}

#[tokio::test]
async fn metrics_overview_returns_count_and_latency() {
    let (app, token) = setup_app().await;

    // Query overview for traces
    let query_param = json!({
        "view": "traces",
        "fromTimestamp": "2000-01-01T00:00:00Z",
        "toTimestamp": "3000-01-01T00:00:00Z",
        "metrics": [{"measure": "count", "aggregation": "count"}]
    });

    let uri = format!(
        "/api/public/metrics?query={}",
        url_encode_json(&query_param)
    );
    let response = app
        .clone()
        .oneshot(authed_request("GET", &uri, &token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Query overview for error observations
    let error_query = json!({
        "view": "observations",
        "fromTimestamp": "2000-01-01T00:00:00Z",
        "toTimestamp": "3000-01-01T00:00:00Z",
        "filters": [{"column": "level", "value": "ERROR"}]
    });
    let error_uri = format!(
        "/api/public/metrics?query={}",
        url_encode_json(&error_query)
    );
    let error_response = app
        .oneshot(authed_request("GET", &error_uri, &token, None))
        .await
        .unwrap();
    assert_eq!(error_response.status(), StatusCode::OK);
}

async fn setup_mock_app() -> (axum::Router, String) {
    let token = "mock-test-token".to_string();
    let router = xtrace::test_app::setup_mock_router(&token).await;
    (router, token)
}

#[tokio::test]
async fn in_memory_storage_integration_test() {
    let (app, token) = setup_mock_app().await;
    let trace_id = Uuid::new_v4();
    let obs_id = Uuid::new_v4();

    // 1. Ingest a trace and an observation
    let ingest = authed_request(
        "POST",
        "/v1/l/batch",
        &token,
        Some(json!({
            "trace": {
                "id": trace_id,
                "timestamp": Utc::now(),
                "name": "in-memory-test",
                "userId": "bob",
                "tags": ["mem-tag"]
            },
            "observations": [{
                "id": obs_id,
                "traceId": trace_id,
                "type": "GENERATION",
                "name": "llm-mock",
                "startTime": Utc::now(),
                "endTime": Utc::now(),
                "model": "llama-3",
                "input": {"prompt": "ping"},
                "output": {"response": "pong"},
                "level": "ERROR",
                "statusMessage": "Failed intentionally"
            }]
        })),
    );

    let ingest_response = app.clone().oneshot(ingest).await.unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // 2. Fetch trace list
    let list_response = app
        .clone()
        .oneshot(authed_request(
            "GET",
            "/api/public/traces?page=1&limit=10&fields=io,observations,metrics",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_data: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(list_data["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        list_data["data"][0]["name"].as_str(),
        Some("in-memory-test")
    );

    // 3. Fetch trace detail
    let detail_response = app
        .clone()
        .oneshot(authed_request(
            "GET",
            &format!("/api/public/traces/{trace_id}"),
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_bytes = axum::body::to_bytes(detail_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let detail_data: Value = serde_json::from_slice(&detail_bytes).unwrap();
    assert_eq!(detail_data["name"].as_str(), Some("in-memory-test"));
    assert_eq!(detail_data["observations"].as_array().unwrap().len(), 1);
    assert_eq!(
        detail_data["observations"][0]["name"].as_str(),
        Some("llm-mock")
    );

    // 4. Ingest a metric point
    let write_response = app
        .clone()
        .oneshot(authed_request(
            "POST",
            "/v1/metrics/batch",
            &token,
            Some(json!({
                "metrics": [{
                    "name": "cpu_load",
                    "labels": {"host": "localhost"},
                    "value": 75.0,
                    "timestamp": Utc::now()
                }]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(write_response.status(), StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // 5. Query metrics names
    let names_response = app
        .clone()
        .oneshot(authed_request(
            "GET",
            "/api/public/metrics/names",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(names_response.status(), StatusCode::OK);
    let names_bytes = axum::body::to_bytes(names_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let names_data: Value = serde_json::from_slice(&names_bytes).unwrap();
    assert!(names_data["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("cpu_load")));

    // 6. Query metrics values
    let query_response = app
        .clone()
        .oneshot(authed_request(
            "GET",
            "/api/public/metrics/query?name=cpu_load&step=1m&agg=avg",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(query_response.status(), StatusCode::OK);
    let query_bytes = axum::body::to_bytes(query_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let query_data: Value = serde_json::from_slice(&query_bytes).unwrap();
    assert_eq!(query_data["data"].as_array().unwrap().len(), 1);
    assert_eq!(query_data["data"][0]["values"].as_array().unwrap().len(), 1);
    assert_eq!(
        query_data["data"][0]["values"][0]["value"].as_f64(),
        Some(75.0)
    );

    // 7. Query daily metrics
    let daily_response = app
        .clone()
        .oneshot(authed_request(
            "GET",
            "/api/public/metrics/daily",
            &token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(daily_response.status(), StatusCode::OK);
    let daily_bytes = axum::body::to_bytes(daily_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let daily_data: Value = serde_json::from_slice(&daily_bytes).unwrap();
    assert_eq!(daily_data["data"].as_array().unwrap().len(), 1);
    assert_eq!(daily_data["data"][0]["countTraces"].as_i64(), Some(1));

    // 8. Query overview with traces view and observations view
    let query_param = json!({
        "view": "traces",
        "fromTimestamp": "2000-01-01T00:00:00Z",
        "toTimestamp": "3000-01-01T00:00:00Z",
        "metrics": [{"measure": "count", "aggregation": "count"}]
    });
    let uri = format!(
        "/api/public/metrics?query={}",
        url_encode_json(&query_param)
    );
    let response = app
        .clone()
        .oneshot(authed_request("GET", &uri, &token, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let overview_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let overview_data: Value = serde_json::from_slice(&overview_bytes).unwrap();
    assert_eq!(overview_data["data"][0]["count_count"].as_i64(), Some(1));

    let error_query = json!({
        "view": "observations",
        "fromTimestamp": "2000-01-01T00:00:00Z",
        "toTimestamp": "3000-01-01T00:00:00Z",
        "filters": [{"column": "level", "value": "ERROR"}]
    });
    let error_uri = format!(
        "/api/public/metrics?query={}",
        url_encode_json(&error_query)
    );
    let error_response = app
        .oneshot(authed_request("GET", &error_uri, &token, None))
        .await
        .unwrap();
    assert_eq!(error_response.status(), StatusCode::OK);
    let error_bytes = axum::body::to_bytes(error_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let error_data: Value = serde_json::from_slice(&error_bytes).unwrap();
    assert_eq!(
        error_data["data"][0]["traceId"].as_str(),
        Some(trace_id.to_string().as_str())
    );
}
