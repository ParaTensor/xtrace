//! `tracing::Layer` for automatic metric collection and trace ID propagation.
//!
//! When the `tracing` feature is enabled, add `XtraceLayer` to your subscriber
//! to automatically push metrics from tracing events and span durations to xtrace,
//! and propagate a unique Trace ID across the span tree.
//!
//! # Example
//!
//! ```ignore
//! use tracing_subscriber::layer::SubscriberExt;
//! use tracing_subscriber::util::SubscriberInitExt;
//! use xtrace_client::{Client, XtraceLayer, current_trace_id};
//!
//! let client = Client::new("http://127.0.0.1:8742/", "token")?;
//! let layer = XtraceLayer::new(client);
//!
//! tracing_subscriber::registry()
//!     .with(layer)
//!     .with(tracing_subscriber::fmt::layer())
//!     .init();
//!
//! // Events with metric= and value= are auto-pushed:
//! tracing::info!(metric = "zene_tokens", value = 100, model = "gpt-4");
//!
//! // Trace ID is available anywhere inside a span:
//! if let Some(tid) = current_trace_id() {
//!     println!("trace_id = {}", tid);
//! }
//!
//! // Span durations are auto-reported as span_duration metric:
//! #[tracing::instrument(fields(session_id = %session.id))]
//! async fn execute_tool() {
//!     // ...
//! }
//! ```

use chrono::Utc;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;
use tracing::field::Field;
use tracing::field::Visit;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use uuid::Uuid;

use crate::Client;
use crate::MetricPoint;

/// Default metric name for span duration.
pub const SPAN_DURATION_METRIC: &str = "span_duration";

const BATCH_SIZE: usize = 50;
const FLUSH_INTERVAL_MS: u64 = 500;

const LABEL_FIELDS: &[&str] = &[
    "session_id",
    "task_id",
    "model",
    "model_name",
    "provider",
    "agent_role",
    "tool_name",
    "status",
];

// ---------------------------------------------------------------------------
// Global trace-id store
// ---------------------------------------------------------------------------

fn trace_id_store() -> &'static Mutex<HashMap<u64, Uuid>> {
    static STORE: OnceLock<Mutex<HashMap<u64, Uuid>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the trace ID associated with the **current** tracing span, if any.
///
/// When `XtraceLayer` is active, every root span gets a new `Uuid` trace ID
/// and all child spans inherit it. Use this to propagate the trace ID into
/// outgoing HTTP headers, logs, or any other context.
///
/// Returns `None` if no span is currently entered or `XtraceLayer` is not installed.
///
/// ```ignore
/// if let Some(tid) = xtrace_client::current_trace_id() {
///     headers.insert("X-Trace-Id", tid.to_string().parse().unwrap());
/// }
/// ```
pub fn current_trace_id() -> Option<Uuid> {
    let span = tracing::Span::current();
    let id = span.id()?;
    let key = id.into_u64();
    trace_id_store().lock().ok()?.get(&key).copied()
}

// ---------------------------------------------------------------------------
// XtraceLayer
// ---------------------------------------------------------------------------

/// A tracing Layer that pushes metrics to xtrace and propagates trace IDs.
///
/// - **Events** with `metric` and `value` fields are pushed as metrics.
///   Other string/numeric fields (session_id, task_id, model, etc.) become labels.
/// - **Span durations** are reported as `span_duration` metric with `span_name` label.
/// - **Trace IDs** are auto-generated for root spans and inherited by children.
///   The `trace_id` is injected into every metric's labels automatically.
///   If a span carries an explicit `trace_id` field, that value is used instead.
#[derive(Clone)]
pub struct XtraceLayer {
    inner: Arc<XtraceLayerInner>,
}

struct XtraceLayerInner {
    tx: mpsc::SyncSender<MetricPoint>,
}

struct SpanRecord {
    created_at: Instant,
    name: String,
}

impl XtraceLayer {
    /// Create a new XtraceLayer and spawn the background flusher.
    pub fn new(client: Client) -> Self {
        let (tx, rx) = mpsc::sync_channel(1000);
        let inner = Arc::new(XtraceLayerInner { tx });

        let client = client.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(r) => r,
                Err(_) => return,
            };
            let mut batch = Vec::with_capacity(BATCH_SIZE);
            let mut last_flush = Instant::now();
            let flush_interval = std::time::Duration::from_millis(FLUSH_INTERVAL_MS);

            loop {
                match rx.recv_timeout(flush_interval) {
                    Ok(point) => {
                        batch.push(point);
                        if batch.len() >= BATCH_SIZE {
                            flush_batch(&rt, &client, &mut batch);
                            last_flush = Instant::now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !batch.is_empty() && last_flush.elapsed() >= flush_interval {
                            flush_batch(&rt, &client, &mut batch);
                            last_flush = Instant::now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if !batch.is_empty() {
                flush_batch(&rt, &client, &mut batch);
            }
        });

        Self { inner }
    }

    fn try_send(&self, point: MetricPoint) {
        let _ = self.inner.tx.try_send(point);
    }
}

fn flush_batch(rt: &tokio::runtime::Runtime, client: &Client, batch: &mut Vec<MetricPoint>) {
    if batch.is_empty() {
        return;
    }
    let points = std::mem::take(batch);
    let client = client.clone();
    rt.block_on(async move {
        let _ = client.push_metrics(&points).await;
    });
}

// ---------------------------------------------------------------------------
// Layer implementation
// ---------------------------------------------------------------------------

impl<S> Layer<S> for XtraceLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: Context<'_, S>,
    ) {
        let name = attrs.metadata().name().to_string();
        let key = id.clone().into_u64();

        // Check for an explicit trace_id field on this span.
        let mut tid_visitor = TraceIdVisitor::default();
        attrs.record(&mut tid_visitor);

        let trace_id = if let Some(explicit) = tid_visitor.trace_id {
            explicit
        } else {
            // Inherit from parent, or generate a new one for root spans.
            let parent_id = if attrs.is_root() {
                None
            } else if let Some(parent) = attrs.parent() {
                Some(parent.clone())
            } else {
                ctx.current_span().id().cloned()
            };

            let parent_tid = parent_id
                .and_then(|pid| trace_id_store().lock().ok()?.get(&pid.into_u64()).copied());

            parent_tid.unwrap_or_else(Uuid::new_v4)
        };

        // Store trace_id mapping.
        if let Ok(mut store) = trace_id_store().lock() {
            store.insert(key, trace_id);
        }

        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanRecord {
                created_at: Instant::now(),
                name,
            });
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = MetricEventVisitor::default();
        event.record(&mut visitor);
        if let Some(mut point) = visitor.into_metric_point() {
            // Auto-inject trace_id from current span context.
            if let Some(tid) = ctx
                .current_span()
                .id()
                .and_then(|sid| trace_id_store().lock().ok()?.get(&sid.into_u64()).copied())
            {
                point
                    .labels
                    .entry("trace_id".to_string())
                    .or_insert_with(|| tid.to_string());
            }
            self.try_send(point);
        }
    }

    fn on_close(&self, id: tracing::Id, ctx: Context<'_, S>) {
        let key = id.into_u64();

        // Remove and read trace_id before cleanup.
        let trace_id = trace_id_store()
            .lock()
            .ok()
            .and_then(|mut s| s.remove(&key));

        let (duration_secs, span_name) = {
            let opt = ctx
                .span(&id)
                .and_then(|span| span.extensions_mut().remove::<SpanRecord>())
                .map(|rec| (rec.created_at.elapsed().as_secs_f64(), rec.name));
            if let Some(res) = opt {
                res
            } else {
                return;
            }
        };

        let mut labels = HashMap::new();
        labels.insert("span_name".to_string(), span_name);
        if let Some(tid) = trace_id {
            labels.insert("trace_id".to_string(), tid.to_string());
        }

        let point = MetricPoint {
            name: SPAN_DURATION_METRIC.to_string(),
            labels,
            value: duration_secs,
            timestamp: Utc::now(),
        };
        self.try_send(point);
    }
}

// ---------------------------------------------------------------------------
// Visitors
// ---------------------------------------------------------------------------

/// Extracts an explicit `trace_id` field from span attributes.
#[derive(Default)]
struct TraceIdVisitor {
    trace_id: Option<Uuid>,
}

impl Visit for TraceIdVisitor {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "trace_id" || field.name() == "xtrace.trace_id" {
            if let Ok(id) = Uuid::parse_str(value) {
                self.trace_id = Some(id);
            }
        }
    }
}

#[derive(Default)]
struct MetricEventVisitor {
    metric: Option<String>,
    value: Option<f64>,
    labels: HashMap<String, String>,
}

impl MetricEventVisitor {
    fn into_metric_point(self) -> Option<MetricPoint> {
        let metric = self.metric?;
        let value = self.value.unwrap_or(0.0);
        Some(MetricPoint {
            name: metric,
            labels: self.labels,
            value,
            timestamp: Utc::now(),
        })
    }
}

impl Visit for MetricEventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if LABEL_FIELDS.contains(&field.name()) {
            self.labels
                .insert(field.name().to_string(), format!("{:?}", value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "metric" {
            self.metric = Some(value.to_string());
        } else if LABEL_FIELDS.contains(&field.name()) {
            self.labels
                .insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "value" {
            self.value = Some(value);
        } else if LABEL_FIELDS.contains(&field.name()) {
            self.labels
                .insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "value" {
            self.value = Some(value as f64);
        } else if LABEL_FIELDS.contains(&field.name()) {
            self.labels
                .insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "value" {
            self.value = Some(value as f64);
        } else if LABEL_FIELDS.contains(&field.name()) {
            self.labels
                .insert(field.name().to_string(), value.to_string());
        }
    }
}
