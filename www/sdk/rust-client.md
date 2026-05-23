# Rust Client SDK

The `xtrace-client` crate provides an async HTTP client for interacting with the xtrace service.

## Installation

```toml
[dependencies]
xtrace-client = "0.1.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

To enable automatic metric collection via the `tracing` ecosystem:

```toml
[dependencies]
xtrace-client = { version = "0.1.1", features = ["tracing"] }
```

## Quick Start

```rust
use xtrace_client::{Client, TraceListQuery};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::new("http://127.0.0.1:8742/", "your-token")?;

    // Health check
    client.healthz().await?;

    // List traces
    let traces = client.list_traces(&TraceListQuery::default()).await?;
    println!("Found {} traces", traces.data.len());

    Ok(())
}
```

## Ingest Traces

```rust
use xtrace_client::{Client, BatchIngestRequest, TraceIngest, ObservationIngest};
use uuid::Uuid;
use chrono::Utc;

let client = Client::new("http://127.0.0.1:8742/", "your-token")?;

let trace_id = Uuid::new_v4();
let obs_id = Uuid::new_v4();

client.ingest_batch(&BatchIngestRequest {
    trace: Some(TraceIngest {
        id: trace_id,
        timestamp: Some(Utc::now()),
        name: Some("agent-task".into()),
        version: Some("v2.1".into()),
        session_id: Some("sess-001".into()),
        ..Default::default()
    }),
    observations: vec![ObservationIngest {
        id: obs_id,
        trace_id,
        r#type: Some("GENERATION".into()),
        name: Some("plan-step".into()),
        model: Some("gpt-4".into()),
        start_time: Some(Utc::now()),
        metadata: Some(serde_json::json!({"agent_role": "Planner"})),
        prompt_tokens: Some(150),
        completion_tokens: Some(300),
        total_tokens: Some(450),
        ..Default::default()
    }],
}).await?;
```

## Push Metrics

```rust
use xtrace_client::{Client, MetricPoint};
use std::collections::HashMap;
use chrono::Utc;

let client = Client::new("http://127.0.0.1:8742/", "your-token")?;

client.push_metrics(&[
    MetricPoint {
        name: "token_usage".into(),
        labels: HashMap::from([
            ("session_id".into(), "sess-001".into()),
            ("model_name".into(), "gpt-4".into()),
            ("agent_role".into(), "Planner".into()),
        ]),
        value: 450.0,
        timestamp: Utc::now(),
    },
]).await?;
```

## Query Metrics

```rust
use xtrace_client::{Client, MetricsQueryParams};

let client = Client::new("http://127.0.0.1:8742/", "your-token")?;

// P90 latency grouped by model
let result = client.query_metrics(&MetricsQueryParams {
    name: "latency".into(),
    agg: Some("p90".into()),
    step: Some("1h".into()),
    group_by: Some("model_name".into()),
    ..Default::default()
}).await?;

for series in &result.data {
    println!("{}: {} points", series.labels, series.values.len());
}
```

## Query Traces

```rust
use xtrace_client::{Client, TraceListQuery};

let client = Client::new("http://127.0.0.1:8742/", "your-token")?;

// Filter traces by version for trend analysis
let traces = client.list_traces(&TraceListQuery {
    version: Some("v2.1".into()),
    order_by: Some("timestamp.desc".into()),
    limit: Some(100),
    ..Default::default()
}).await?;
```

## Daily Metrics

```rust
use xtrace_client::{Client, MetricsDailyQuery};

let client = Client::new("http://127.0.0.1:8742/", "your-token")?;

// Compare daily metrics for a specific version
let daily = client.metrics_daily(&MetricsDailyQuery {
    version: Some("v2.1".into()),
    ..Default::default()
}).await?;

for item in &daily.data {
    println!("{}: {} traces, cost={}", item.date, item.count_traces, item.total_cost);
}
```

## tracing Integration

With the `tracing` feature enabled, `XtraceLayer` automatically pushes metrics from tracing events and span durations — no manual `push_metrics` calls needed.

```rust
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use xtrace_client::{Client, XtraceLayer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::new("http://127.0.0.1:8742/", "your-token")?;

    tracing_subscriber::registry()
        .with(XtraceLayer::new(client))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Events with metric= and value= are pushed automatically.
    // Supported label fields: session_id, task_id, model, model_name,
    //   provider, agent_role, tool_name, status
    tracing::info!(metric = "zene_tokens", value = 512, model = "gpt-4o");

    // Span durations are auto-reported as `span_duration` with `span_name` label.
    let _span = tracing::info_span!("execute_tool").entered();
    // ... work happens here ...
    drop(_span); // duration is recorded and pushed on drop

    Ok(())
}
```

Metrics are batched (≤50 per flush, or every 500 ms) and flushed from a background thread — the tracing hot path is never blocked.

See the [tracing integration guide](/integrations/tracing) for more detail.

## API Reference

### Client

| Method | Description |
|--------|-------------|
| `Client::new(base_url, token)` | Create a new client |
| `healthz()` | Health check |
| `ingest_batch(req)` | Batch ingest traces and observations |
| `list_traces(query)` | List traces with pagination and filters |
| `get_trace(id)` | Get trace detail with observations |
| `push_metrics(points)` | Write metric data points |
| `query_metrics(params)` | Query time-series metrics |
| `list_metric_names()` | List available metric names |
| `metrics_daily(query)` | Daily aggregated metrics |
