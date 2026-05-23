# tracing Integration

`xtrace-client` ships an optional `tracing::Layer` implementation (`XtraceLayer`) that automatically collects metrics from your existing `tracing` instrumentation and pushes them to xtrace — with zero changes to your application logic.

## Installation

```toml
[dependencies]
xtrace-client = { version = "0.1.1", features = ["tracing"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Setup

Register `XtraceLayer` once at startup alongside any other layers:

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
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Your application code ...
    Ok(())
}
```

## Collecting Metrics from Events

Any `tracing` event that includes `metric` and `value` fields is automatically pushed as a metric data point:

```rust
// Minimum required: metric name + value
tracing::info!(metric = "token_count", value = 512_u64);

// With label fields for richer querying
tracing::info!(
    metric = "token_count",
    value = 512_u64,
    model = "gpt-4o",
    session_id = %session.id,
    agent_role = "Planner",
);
```

**Supported label fields** (other fields are ignored):

| Field | Description |
|-------|-------------|
| `session_id` | Session identifier |
| `task_id` | Task identifier |
| `model` / `model_name` | Model name |
| `provider` | Model provider |
| `agent_role` | Agent role (Planner, Executor, etc.) |
| `tool_name` | Tool being invoked |
| `status` | Outcome status |

## Collecting Span Durations

Every span is automatically timed. On close, the duration is pushed as a `span_duration` metric (in seconds) with the `span_name` label set to the span's name.

```rust
// Manual span
{
    let _span = tracing::info_span!("llm_call", model = "gpt-4o").entered();
    call_llm().await?;
} // → span_duration{span_name="llm_call"} pushed here

// With #[tracing::instrument]
#[tracing::instrument(fields(session_id = %req.session_id, model = %req.model))]
async fn execute_tool(req: &ToolRequest) -> Result<ToolOutput> {
    // → span_duration{span_name="execute_tool"} pushed on return
}
```

Query p99 span duration grouped by span name:

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:8742/api/public/metrics/query?name=span_duration&agg=p99&step=5m&group_by=span_name"
```

## Batching and Performance

- Metrics are buffered in a `mpsc::sync_channel` (capacity 1000).
- A background thread flushes batches of ≤50 points or on a 500 ms timer.
- `try_send` is used on the hot path — events are silently dropped if the buffer is full rather than blocking the caller.
- The background thread runs its own `tokio` runtime so it doesn't interfere with your application's executor.

## Integration with `#[tracing::instrument]`

The most ergonomic pattern is to annotate async functions with `#[tracing::instrument]` and emit metric events inside them:

```rust
#[tracing::instrument(fields(session_id = %session.id, model = %request.model))]
async fn plan_step(session: &Session, request: &PlanRequest) -> Result<Plan> {
    let plan = llm_call(request).await?;

    // Token usage is automatically labeled with session_id and model from the span
    tracing::info!(
        metric = "token_count",
        value = plan.usage.total_tokens as f64,
        agent_role = "Planner",
    );

    Ok(plan)
}
```
