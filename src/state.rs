use chrono::{DateTime, Utc};
use dashmap::DashMap;
use governor::{clock::DefaultClock, Quota};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::PgPool;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{http::metrics::MetricsBatchRequest, ingest::batch::BatchIngestRequest};

pub type KeyedRateLimiter =
    governor::RateLimiter<String, governor::state::keyed::DashMapStateStore<String>, DefaultClock>;

pub struct RateLimitStats {
    pub total_allowed: AtomicU64,
    pub total_rejected: AtomicU64,
    pub per_token_rejected: dashmap::DashMap<String, u64>,
}

pub struct IngestStats {
    pub enqueued: AtomicU64,
    pub queue_rejected: AtomicU64,
    pub batches_written: AtomicU64,
    pub batches_failed: AtomicU64,
    pub items_written: AtomicU64,
}

impl Default for IngestStats {
    fn default() -> Self {
        Self::new()
    }
}

impl IngestStats {
    pub fn new() -> Self {
        Self {
            enqueued: AtomicU64::new(0),
            queue_rejected: AtomicU64::new(0),
            batches_written: AtomicU64::new(0),
            batches_failed: AtomicU64::new(0),
            items_written: AtomicU64::new(0),
        }
    }

    pub fn record_enqueued(&self, count: u64) {
        self.enqueued.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_queue_rejected(&self, count: u64) {
        self.queue_rejected.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_batches_written(&self, count: u64) {
        self.batches_written.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_batches_failed(&self, count: u64) {
        self.batches_failed.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_items_written(&self, count: u64) {
        self.items_written.fetch_add(count, Ordering::Relaxed);
    }
}

impl Default for RateLimitStats {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitStats {
    pub fn new() -> Self {
        Self {
            total_allowed: AtomicU64::new(0),
            total_rejected: AtomicU64::new(0),
            per_token_rejected: dashmap::DashMap::new(),
        }
    }

    pub fn record_allowed(&self) {
        self.total_allowed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rejected(&self, masked_key: &str) {
        self.total_rejected.fetch_add(1, Ordering::Relaxed);
        self.per_token_rejected
            .entry(masked_key.to_string())
            .and_modify(|c| *c += 1)
            .or_insert(1);
    }
}

pub fn mask_client_key(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("bearer:") {
        if rest.len() > 8 {
            format!("bearer:{}***", &rest[..8])
        } else {
            format!("bearer:{rest}***")
        }
    } else {
        key.to_string()
    }
}

pub struct ServerConfig {
    pub database_url: String,
    pub api_bearer_token: String,
    pub bind_addr: String,
    pub default_project_id: String,
    pub langfuse_public_key: Option<String>,
    pub langfuse_secret_key: Option<String>,
    pub rate_limit_qps: u32,
    pub rate_limit_burst: u32,
    /// When true, allows unauthenticated access to `GET /api/public/projects` and
    /// `POST /api/public/otel/v1/traces` if Langfuse public/secret keys are not configured.
    /// **Must stay false in production** (default).
    pub allow_unauthenticated_compat: bool,
    /// Maximum HTTP request body size in bytes (ingest endpoints).
    pub max_request_body_bytes: usize,
    /// Directory for Langfuse-compatible media file storage.
    pub media_dir: PathBuf,
    /// Optional public base URL (e.g. `https://xtrace.example.com`) for media download links.
    pub public_base_url: Option<String>,
    /// Maximum allowed media upload size in bytes.
    pub media_max_content_length: usize,
}

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub api_bearer_token: Arc<str>,
    pub langfuse_public_key: Option<Arc<str>>,
    pub langfuse_secret_key: Option<Arc<str>>,
    pub default_project_id: Arc<str>,
    pub(crate) ingest_tx: mpsc::Sender<BatchIngestRequest>,
    pub(crate) metrics_tx: mpsc::Sender<MetricsBatchRequest>,
    pub query_limiter: Arc<KeyedRateLimiter>,
    pub rate_limit_stats: Arc<RateLimitStats>,
    pub ingest_stats: Arc<IngestStats>,
    pub rate_limit_qps: u32,
    pub rate_limit_burst: u32,
    pub allow_unauthenticated_compat: bool,
    pub media_dir: Arc<PathBuf>,
    pub public_base_url: Option<Arc<str>>,
    pub media_max_content_length: usize,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MediaRow {
    pub id: String,
    pub project_id: String,
    pub content_type: String,
    pub content_length: i64,
    pub sha256_hash: String,
    pub uploaded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl AppState {
    pub fn build_limiter(qps: u32, burst: u32) -> Arc<KeyedRateLimiter> {
        let quota = Quota::per_second(NonZeroU32::new(qps).expect("rate_limit_qps must be > 0"))
            .allow_burst(NonZeroU32::new(burst).expect("rate_limit_burst must be > 0"));
        Arc::new(KeyedRateLimiter::keyed(quota))
    }
}

#[derive(Clone)]
pub enum DatabaseConnection {
    Postgres(PgPool),
    Memory(Arc<MemoryDb>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TraceRow {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub name: Option<String>,
    pub input: Option<JsonValue>,
    pub output: Option<JsonValue>,
    pub session_id: Option<String>,
    pub release: Option<String>,
    pub version: Option<String>,
    pub user_id: Option<String>,
    pub metadata: Option<JsonValue>,
    pub tags: Vec<String>,
    pub public: bool,
    pub environment: String,
    pub latency: Option<f64>,
    pub total_cost: Option<f64>,
    pub external_id: Option<String>,
    pub bookmarked: bool,
    pub project_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ObservationRow {
    pub id: Uuid,
    pub trace_id: Uuid,
    pub r#type: String,
    pub name: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub completion_start_time: Option<DateTime<Utc>>,
    pub model: Option<String>,
    pub model_parameters: Option<JsonValue>,
    pub input: Option<JsonValue>,
    pub output: Option<JsonValue>,
    pub usage: Option<JsonValue>,
    pub level: Option<String>,
    pub status_message: Option<String>,
    pub parent_observation_id: Option<Uuid>,
    pub prompt_id: Option<String>,
    pub prompt_name: Option<String>,
    pub prompt_version: Option<String>,
    pub model_id: Option<String>,
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    pub total_price: Option<f64>,
    pub calculated_input_cost: Option<f64>,
    pub calculated_output_cost: Option<f64>,
    pub calculated_total_cost: Option<f64>,
    pub latency: Option<f64>,
    pub time_to_first_token: Option<f64>,
    pub completion_tokens: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub unit: Option<String>,
    pub metadata: Option<JsonValue>,
    pub environment: String,
    pub project_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MetricRow {
    pub project_id: String,
    pub environment: String,
    pub name: String,
    pub labels: serde_json::Value,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

pub struct MemoryDb {
    pub traces: DashMap<Uuid, TraceRow>,
    pub observations: DashMap<Uuid, ObservationRow>,
    pub metrics: Mutex<Vec<MetricRow>>,
    /// Key: `{project_id}:{media_id}`
    pub media: DashMap<String, MediaRow>,
    pub data_dir: Option<PathBuf>,
}

impl MemoryDb {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let db = Self {
            traces: DashMap::new(),
            observations: DashMap::new(),
            metrics: Mutex::new(Vec::new()),
            media: DashMap::new(),
            data_dir,
        };
        db.load();
        db
    }

    fn load(&self) {
        let Some(ref dir) = self.data_dir else {
            return;
        };

        let traces_path = dir.join("traces.json");
        if traces_path.exists() {
            if let Ok(file) = std::fs::File::open(&traces_path) {
                if let Ok(data) = serde_json::from_reader::<_, Vec<TraceRow>>(file) {
                    for r in data {
                        self.traces.insert(r.id, r);
                    }
                }
            }
        }

        let obs_path = dir.join("observations.json");
        if obs_path.exists() {
            if let Ok(file) = std::fs::File::open(&obs_path) {
                if let Ok(data) = serde_json::from_reader::<_, Vec<ObservationRow>>(file) {
                    for r in data {
                        self.observations.insert(r.id, r);
                    }
                }
            }
        }

        let metrics_path = dir.join("metrics.json");
        if metrics_path.exists() {
            if let Ok(file) = std::fs::File::open(&metrics_path) {
                if let Ok(data) = serde_json::from_reader::<_, Vec<MetricRow>>(file) {
                    if let Ok(mut m) = self.metrics.lock() {
                        *m = data;
                    }
                }
            }
        }
    }

    pub fn save(&self) {
        let Some(ref dir) = self.data_dir else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);

        let traces_path = dir.join("traces.json");
        if let Ok(file) = std::fs::File::create(&traces_path) {
            let data: Vec<TraceRow> = self
                .traces
                .iter()
                .map(|item| item.value().clone())
                .collect();
            let _ = serde_json::to_writer_pretty(file, &data);
        }

        let obs_path = dir.join("observations.json");
        if let Ok(file) = std::fs::File::create(&obs_path) {
            let data: Vec<ObservationRow> = self
                .observations
                .iter()
                .map(|item| item.value().clone())
                .collect();
            let _ = serde_json::to_writer_pretty(file, &data);
        }

        let metrics_path = dir.join("metrics.json");
        if let Ok(file) = std::fs::File::create(&metrics_path) {
            if let Ok(m) = self.metrics.lock() {
                let _ = serde_json::to_writer_pretty(file, &*m);
            }
        }
    }

    pub fn spawn_sync_loop(self: Arc<Self>) {
        if self.data_dir.is_none() {
            return;
        }
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                self.save();
            }
        });
    }
}
