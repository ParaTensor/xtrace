# xtrace-client

Rust HTTP SDK for the [xtrace](https://github.com/lipish/xtrace) observability service.

[![crates.io](https://img.shields.io/crates/v/xtrace-client.svg)](https://crates.io/crates/xtrace-client)

## Installation

```toml
[dependencies]
xtrace-client = "0.1.1"
```

To enable automatic metric collection via `tracing`:

```toml
[dependencies]
xtrace-client = { version = "0.1.1", features = ["tracing"] }
```

## Basic Usage

```rust
use xtrace_client::{Client, TraceListQuery, MetricPoint};
use std::collections::HashMap;
use chrono::Utc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::new("http://127.0.0.1:8742/", "YOUR_TOKEN")?;

    // Health check
    client.healthz().await?;

    // List traces
    let traces = client.list_traces(&TraceListQuery::default()).await?;
    println!("Total traces: {}", traces.meta.total_items);

    // Push a custom metric
    client.push_metrics(&[MetricPoint {
        name: "gpu_utilization".to_string(),
        labels: HashMap::from([
            ("node_id".to_string(), "node-1".to_string()),
        ]),
        value: 85.0,
        timestamp: Utc::now(),
    }]).await?;

    Ok(())
}
```

## tracing Integration (feature = "tracing")

`XtraceLayer` is a `tracing::Layer` that automatically pushes metrics to xtrace from tracing events and span durations — no manual push calls needed.

```rust
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use xtrace_client::{Client, XtraceLayer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::new("http://127.0.0.1:8742/", "YOUR_TOKEN")?;

    tracing_subscriber::registry()
        .with(XtraceLayer::new(client))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Events with metric= and value= are auto-pushed as metrics.
    // Additional label fields: session_id, task_id, model, model_name,
    //   provider, agent_role, tool_name, status
    tracing::info!(metric = "zene_tokens", value = 512, model = "gpt-4o");

    // Span durations are auto-reported as the `span_duration` metric
    // with a `span_name` label.
    let _span = tracing::info_span!("llm_call").entered();
    // ... do work ...
    drop(_span); // duration reported on drop

    Ok(())
}
```

Metrics are batched (up to 50 per flush or every 500 ms) and pushed in a background thread — the tracing hot path is never blocked.

## Available Methods

| Method | Endpoint |
|--------|----------|
| `healthz` | `GET /healthz` |
| `ingest_batch` | `POST /v1/l/batch` |
| `list_traces` | `GET /api/public/traces` |
| `get_trace` | `GET /api/public/traces/:id` |
| `metrics_daily` | `GET /api/public/metrics/daily` |
| `push_metrics` | `POST /v1/metrics/batch` |
| `query_metrics` | `GET /api/public/metrics/query` |
| `list_metric_names` | `GET /api/public/metrics/names` |

## Metrics Query Parameters

`query_metrics` supports downsampling and aggregation:

| Parameter | Values | Default |
|-----------|--------|---------|
| `name` | metric name (required) | — |
| `from` / `to` | ISO8601 timestamps | last 1 hour |
| `labels` | JSON label filter | — |
| `step` | `1m` `5m` `1h` `1d` | `1m` |
| `agg` | `avg` `max` `min` `sum` `last` `p50` `p90` `p99` | `avg` |
| `group_by` | label key to group series by | — |

```rust
use xtrace_client::MetricsQueryParams;
use std::collections::HashMap;

let result = client.query_metrics(&MetricsQueryParams {
    name: "zene_tokens".to_string(),
    step: Some("5m".to_string()),
    agg: Some("p99".to_string()),
    group_by: Some("model".to_string()),
    ..Default::default()
}).await?;

for series in &result.data {
    println!("labels={} points={}", series.labels, series.values.len());
}
```
