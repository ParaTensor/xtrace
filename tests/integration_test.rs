use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use chrono::Utc;
use opentelemetry_proto::tonic::{
    collector::metrics::v1::ExportMetricsServiceRequest as PbExportMetricsServiceRequest,
    common::v1::{
        any_value::Value as PbAnyValue, AnyValue as PbAnyValueMsg,
        InstrumentationScope as PbInstrumentationScope, KeyValue as PbKeyValue,
    },
    metrics::v1::{
        metric::Data as PbMetricData, Gauge as PbGauge, Metric as PbMetric,
        NumberDataPoint as PbNumberDataPoint, ResourceMetrics as PbResourceMetrics,
        ScopeMetrics as PbScopeMetrics,
    },
};
use prost::Message;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;
use xtrace::test_app::{setup_mock_router, setup_mock_router_with_tokens, setup_test_router};

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

async fn wait_for_metric_query(app: &axum::Router, token: &str, name: &str, agg: &str) -> Value {
    let uri = format!("/api/public/metrics/query?name={name}&step=1m&agg={agg}");
    for _ in 0..20 {
        let response = app
            .clone()
            .oneshot(authed_request("GET", &uri, token, None))
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let data: Value = serde_json::from_slice(&bytes).unwrap();
            if data["data"]
                .as_array()
                .is_some_and(|series| !series.is_empty())
            {
                return data;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("metric {name} did not appear");
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

async fn metrics_query_response_json(
    app: axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> Value {
    let response = app
        .oneshot(authed_request(method, uri, token, body))
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected response body: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn metrics_query_count_aggregation_round_trip() {
    for (app, token) in [setup_app().await, setup_mock_app().await] {
        let metric_name = format!("count_metric_{}", Uuid::new_v4());
        let now = Utc::now();

        for _ in 0..2 {
            let write_response = app
                .clone()
                .oneshot(authed_request(
                    "POST",
                    "/v1/metrics/batch",
                    &token,
                    Some(json!({
                        "metrics": [{
                            "name": metric_name,
                            "labels": {"node_id": "node-1"},
                            "value": 1.0,
                            "timestamp": now
                        }]
                    })),
                ))
                .await
                .unwrap();
            assert_eq!(write_response.status(), StatusCode::OK);
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let query_data = metrics_query_response_json(
            app,
            "GET",
            &format!("/api/public/metrics/query?name={metric_name}&step=1m&agg=count"),
            &token,
            None,
        )
        .await;

        assert_eq!(query_data["meta"]["series_count"].as_u64(), Some(1));
        assert_eq!(query_data["data"].as_array().unwrap().len(), 1);
        assert_eq!(query_data["data"][0]["values"].as_array().unwrap().len(), 1);
        assert_eq!(
            query_data["data"][0]["values"][0]["value"].as_f64(),
            Some(2.0)
        );
    }
}

#[tokio::test]
async fn metrics_query_multi_key_group_by_round_trip() {
    for (app, token) in [setup_app().await, setup_mock_app().await] {
        let metric_name = format!("group_metric_{}", Uuid::new_v4());
        let now = Utc::now();
        let points = [
            json!({
                "name": metric_name,
                "labels": {"node": "node-a", "region": "us"},
                "value": 1.0,
                "timestamp": now
            }),
            json!({
                "name": metric_name,
                "labels": {"node": "node-a", "region": "us"},
                "value": 2.0,
                "timestamp": now
            }),
            json!({
                "name": metric_name,
                "labels": {"node": "node-a", "region": "eu"},
                "value": 3.0,
                "timestamp": now
            }),
            json!({
                "name": metric_name,
                "labels": {"node": "node-b", "region": "us"},
                "value": 4.0,
                "timestamp": now
            }),
        ];

        for point in points {
            let write_response = app
                .clone()
                .oneshot(authed_request(
                    "POST",
                    "/v1/metrics/batch",
                    &token,
                    Some(json!({ "metrics": [point] })),
                ))
                .await
                .unwrap();
            assert_eq!(write_response.status(), StatusCode::OK);
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let query_data = metrics_query_response_json(
            app,
            "GET",
            &format!(
                "/api/public/metrics/query?name={metric_name}&step=1m&agg=count&group_by=node,region"
            ),
            &token,
            None,
        )
        .await;

        assert_eq!(query_data["meta"]["series_count"].as_u64(), Some(3));
        assert_eq!(query_data["data"].as_array().unwrap().len(), 3);

        let mut series = std::collections::BTreeMap::new();
        for item in query_data["data"].as_array().unwrap() {
            let node = item["labels"]["node"].as_str().unwrap().to_string();
            let region = item["labels"]["region"].as_str().unwrap().to_string();
            let value = item["values"][0]["value"].as_f64().unwrap();
            series.insert((node, region), value);
        }

        assert_eq!(
            series.get(&("node-a".to_string(), "us".to_string())),
            Some(&2.0)
        );
        assert_eq!(
            series.get(&("node-a".to_string(), "eu".to_string())),
            Some(&1.0)
        );
        assert_eq!(
            series.get(&("node-b".to_string(), "us".to_string())),
            Some(&1.0)
        );
    }
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

#[tokio::test]
async fn otlp_metrics_json_round_trip_and_auth() {
    let app = setup_mock_router("metrics-test-token").await;
    let token = "metrics-test-token";

    let unauthorized = Request::builder()
        .method("POST")
        .uri("/api/public/otel/v1/metrics")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .unwrap();
    let unauthorized_response = app.clone().oneshot(unauthorized).await.unwrap();
    assert_eq!(unauthorized_response.status(), StatusCode::UNAUTHORIZED);

    let ts = Utc::now().timestamp_nanos_opt().unwrap().to_string();
    let payload = json!({
        "resourceMetrics": [{
            "resource": {
                "attributes": [
                    {"key": "service.name", "value": {"stringValue": "checkout"}},
                    {"key": "service.instance.id", "value": {"stringValue": "api-1"}}
                ]
            },
            "scopeMetrics": [{
                "scope": {
                    "name": "shim",
                    "version": "1.0.0",
                    "attributes": [
                        {"key": "otel.scope.kind", "value": {"stringValue": "test"}}
                    ]
                },
                "metrics": [
                    {
                        "name": "cpu_temp",
                        "gauge": {
                            "dataPoints": [{
                                "attributes": [
                                    {"key": "room", "value": {"stringValue": "server"}}
                                ],
                                "timeUnixNano": ts,
                                "asDouble": 42.5
                            }]
                        }
                    },
                    {
                        "name": "req_count",
                        "sum": {
                            "dataPoints": [{
                                "attributes": [
                                    {"key": "route", "value": {"stringValue": "/chat"}}
                                ],
                                "timeUnixNano": ts,
                                "asInt": 7
                            }]
                        }
                    }
                ]
            }]
        }]
    });

    let ingest = authed_request("POST", "/api/public/otel/v1/metrics", token, Some(payload));
    let ingest_response = app.clone().oneshot(ingest).await.unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);

    let cpu_data = wait_for_metric_query(&app, token, "cpu_temp", "avg").await;
    assert_eq!(cpu_data["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        cpu_data["data"][0]["values"][0]["value"].as_f64(),
        Some(42.5)
    );
    assert_eq!(cpu_data["data"][0]["labels"]["service.name"], "checkout");
    assert_eq!(cpu_data["data"][0]["labels"]["otel.scope.name"], "shim");
    assert_eq!(cpu_data["data"][0]["labels"]["room"], "server");

    let req_data = wait_for_metric_query(&app, token, "req_count", "avg").await;
    assert_eq!(req_data["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        req_data["data"][0]["values"][0]["value"].as_f64(),
        Some(7.0)
    );
    assert_eq!(req_data["data"][0]["labels"]["route"], "/chat");
    assert_eq!(
        req_data["data"][0]["labels"]["service.instance.id"],
        "api-1"
    );
}

#[tokio::test]
async fn otlp_metrics_protobuf_round_trip() {
    let app = setup_mock_router("metrics-test-token-proto").await;
    let token = "metrics-test-token-proto";
    let ts = Utc::now().timestamp_nanos_opt().unwrap() as u64;

    let request = PbExportMetricsServiceRequest {
        resource_metrics: vec![PbResourceMetrics {
            resource: Some(opentelemetry_proto::tonic::resource::v1::Resource {
                attributes: vec![PbKeyValue {
                    key: "service.name".to_string(),
                    value: Some(PbAnyValueMsg {
                        value: Some(PbAnyValue::StringValue("billing".to_string())),
                    }),
                }],
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            scope_metrics: vec![PbScopeMetrics {
                scope: Some(PbInstrumentationScope {
                    name: "proto-shim".to_string(),
                    version: "2.1.0".to_string(),
                    attributes: vec![],
                    dropped_attributes_count: 0,
                }),
                metrics: vec![PbMetric {
                    name: "latency_ms".to_string(),
                    description: String::new(),
                    unit: String::new(),
                    metadata: vec![],
                    data: Some(PbMetricData::Gauge(PbGauge {
                        data_points: vec![PbNumberDataPoint {
                            attributes: vec![PbKeyValue {
                                key: "endpoint".to_string(),
                                value: Some(PbAnyValueMsg {
                                    value: Some(PbAnyValue::StringValue(
                                        "/v1/chat".to_string(),
                                    )),
                                }),
                            }],
                            start_time_unix_nano: 0,
                            time_unix_nano: ts,
                            exemplars: vec![],
                            flags: 0,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsDouble(88.0),
                            ),
                        }],
                    })),
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    };

    let body = request.encode_to_vec();
    let ingest = Request::builder()
        .method("POST")
        .uri("/api/public/otel/v1/metrics")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/x-protobuf")
        .body(Body::from(body))
        .unwrap();
    let ingest_response = app.clone().oneshot(ingest).await.unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);

    let data = wait_for_metric_query(&app, token, "latency_ms", "avg").await;
    assert_eq!(data["data"].as_array().unwrap().len(), 1);
    assert_eq!(data["data"][0]["values"][0]["value"].as_f64(), Some(88.0));
    assert_eq!(data["data"][0]["labels"]["service.name"], "billing");
    assert_eq!(data["data"][0]["labels"]["otel.scope.name"], "proto-shim");
    assert_eq!(data["data"][0]["labels"]["endpoint"], "/v1/chat");
}

#[tokio::test]
async fn otlp_histogram_emits_count_and_sum() {
    let app = setup_mock_router("metrics-test-token-hist").await;
    let token = "metrics-test-token-hist";
    let ts = Utc::now().timestamp_nanos_opt().unwrap().to_string();

    let payload = json!({
        "resourceMetrics": [{
            "resource": {
                "attributes": [
                    {"key": "service.name", "value": {"stringValue": "hist-svc"}}
                ]
            },
            "scopeMetrics": [{
                "scope": {"name": "hist-shim", "version": "1.0.0"},
                "metrics": [{
                    "name": "request_duration_ms",
                    "histogram": {
                        "dataPoints": [{
                            "attributes": [
                                {"key": "route", "value": {"stringValue": "/search"}}
                            ],
                            "timeUnixNano": ts,
                            "count": 4,
                            "sum": 100.0,
                            "bucketCounts": [1, 2, 1],
                            "explicitBounds": [10.0, 50.0]
                        }]
                    }
                }]
            }]
        }]
    });

    let ingest = authed_request("POST", "/api/public/otel/v1/metrics", token, Some(payload));
    let ingest_response = app.clone().oneshot(ingest).await.unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);

    let count_data = wait_for_metric_query(&app, token, "request_duration_ms_count", "sum").await;
    assert_eq!(count_data["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        count_data["data"][0]["values"][0]["value"].as_f64(),
        Some(4.0)
    );

    let sum_data = wait_for_metric_query(&app, token, "request_duration_ms_sum", "sum").await;
    assert_eq!(sum_data["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        sum_data["data"][0]["values"][0]["value"].as_f64(),
        Some(100.0)
    );
    assert_eq!(sum_data["data"][0]["labels"]["service.name"], "hist-svc");
    assert_eq!(sum_data["data"][0]["labels"]["route"], "/search");
}

#[tokio::test]
async fn read_only_token_cannot_write_metrics() {
    let app = setup_mock_router_with_tokens("write-token", Some("read-token")).await;
    let response = app
        .oneshot(authed_request(
            "POST",
            "/v1/metrics/batch",
            "read-token",
            Some(json!({
                "metrics": [{
                    "name": "blocked_metric",
                    "labels": {"node": "n1"},
                    "value": 1.0,
                    "timestamp": Utc::now()
                }]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn multi_tenant_project_tokens_are_isolated() {
    let app = xtrace::test_app::setup_mock_router_with_project_tokens(
        "team-a:write-a:read-a,team-b:write-b",
    )
    .await;

    let write_response = app
        .clone()
        .oneshot(authed_request(
            "POST",
            "/v1/metrics/batch",
            "write-a",
            Some(json!({
                "metrics": [{
                    "name": "tenant_metric",
                    "labels": {"tenant": "a"},
                    "value": 9.0,
                    "timestamp": Utc::now()
                }]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(write_response.status(), StatusCode::OK);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let team_b_query = app
        .clone()
        .oneshot(authed_request(
            "GET",
            "/api/public/metrics/query?name=tenant_metric&step=1m&agg=avg",
            "write-b",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(team_b_query.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(team_b_query.into_body(), usize::MAX)
        .await
        .unwrap();
    let data: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(data["meta"]["series_count"].as_u64(), Some(0));

    let team_a_query = app
        .oneshot(authed_request(
            "GET",
            "/api/public/metrics/query?name=tenant_metric&step=1m&agg=avg",
            "write-a",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(team_a_query.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(team_a_query.into_body(), usize::MAX)
        .await
        .unwrap();
    let data: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(data["meta"]["series_count"].as_u64(), Some(1));
}
