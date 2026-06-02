use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use sqlx::{PgPool, QueryBuilder};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::{sync::mpsc, time::Duration};
use uuid::Uuid;

use crate::{
    http::{common::ApiResponse, error::ApiError},
    state::{AppState, IngestStats},
};

#[derive(Debug, Deserialize)]
pub(crate) struct BatchIngestRequest {
    #[serde(default)]
    pub trace: Option<TraceIngest>,
    #[serde(default)]
    pub observations: Vec<ObservationIngest>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct TraceIngest {
    pub id: Uuid,
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,

    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input: Option<JsonValue>,
    #[serde(default)]
    pub output: Option<JsonValue>,
    #[serde(default, alias = "sessionId")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub userId: Option<String>,
    #[serde(default)]
    pub metadata: Option<JsonValue>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub externalId: Option<String>,
    #[serde(default)]
    pub bookmarked: Option<bool>,

    #[serde(default)]
    pub latency: Option<f64>,
    #[serde(default)]
    pub totalCost: Option<f64>,

    #[serde(default)]
    pub projectId: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct ObservationIngest {
    pub id: Uuid,
    pub traceId: Uuid,

    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub startTime: Option<DateTime<Utc>>,
    #[serde(default)]
    pub endTime: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completionStartTime: Option<DateTime<Utc>>,

    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub modelParameters: Option<JsonValue>,

    #[serde(default)]
    pub input: Option<JsonValue>,
    #[serde(default)]
    pub output: Option<JsonValue>,

    #[serde(default)]
    pub usage: Option<JsonValue>,

    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub statusMessage: Option<String>,
    #[serde(default)]
    pub parentObservationId: Option<Uuid>,

    #[serde(default)]
    pub promptId: Option<String>,
    #[serde(default)]
    pub promptName: Option<String>,
    #[serde(default)]
    pub promptVersion: Option<String>,

    #[serde(default)]
    pub modelId: Option<String>,

    #[serde(default)]
    pub inputPrice: Option<f64>,
    #[serde(default)]
    pub outputPrice: Option<f64>,
    #[serde(default)]
    pub totalPrice: Option<f64>,

    #[serde(default)]
    pub calculatedInputCost: Option<f64>,
    #[serde(default)]
    pub calculatedOutputCost: Option<f64>,
    #[serde(default)]
    pub calculatedTotalCost: Option<f64>,

    #[serde(default)]
    pub latency: Option<f64>,
    #[serde(default)]
    pub timeToFirstToken: Option<f64>,

    #[serde(default)]
    pub completionTokens: Option<i64>,
    #[serde(default)]
    pub promptTokens: Option<i64>,
    #[serde(default)]
    pub totalTokens: Option<i64>,
    #[serde(default)]
    pub unit: Option<String>,

    #[serde(default)]
    pub metadata: Option<JsonValue>,

    #[serde(default)]
    pub environment: Option<String>,

    #[serde(default)]
    pub projectId: Option<String>,
}

struct ResolvedTrace {
    id: Uuid,
    project_id: String,
    environment: String,
    timestamp: DateTime<Utc>,
    name: Option<String>,
    input: Option<JsonValue>,
    output: Option<JsonValue>,
    session_id: Option<String>,
    release: Option<String>,
    version: Option<String>,
    user_id: Option<String>,
    metadata: Option<JsonValue>,
    tags: Vec<String>,
    public: bool,
    external_id: Option<String>,
    bookmarked: bool,
    latency: Option<f64>,
    total_cost: Option<f64>,
}

struct ResolvedObservation {
    id: Uuid,
    trace_id: Uuid,
    project_id: String,
    environment: String,
    obs_type: String,
    name: Option<String>,
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
    completion_start_time: Option<DateTime<Utc>>,
    model: Option<String>,
    model_parameters: Option<JsonValue>,
    input: Option<JsonValue>,
    output: Option<JsonValue>,
    usage: Option<JsonValue>,
    level: Option<String>,
    status_message: Option<String>,
    parent_observation_id: Option<Uuid>,
    prompt_id: Option<String>,
    prompt_name: Option<String>,
    prompt_version: Option<String>,
    model_id: Option<String>,
    input_price: Option<f64>,
    output_price: Option<f64>,
    total_price: Option<f64>,
    calculated_input_cost: Option<f64>,
    calculated_output_cost: Option<f64>,
    calculated_total_cost: Option<f64>,
    latency: Option<f64>,
    time_to_first_token: Option<f64>,
    completion_tokens: Option<i64>,
    prompt_tokens: Option<i64>,
    total_tokens: Option<i64>,
    unit: Option<String>,
    metadata: Option<JsonValue>,
}

pub(crate) async fn post_batch(
    State(state): State<AppState>,
    Json(payload): Json<BatchIngestRequest>,
) -> Result<impl IntoResponse, ApiError> {
    match state.ingest_tx.try_send(payload) {
        Ok(()) => {
            state.ingest_stats.record_enqueued(1);
            Ok((
                StatusCode::OK,
                Json(ApiResponse::<serde_json::Value> {
                    message: "Request Successful.".to_string(),
                    code: None,
                    data: None,
                }),
            ))
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            state.ingest_stats.record_queue_rejected(1);
            Err(ApiError::TooManyRequests)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(ApiError::ServiceUnavailable),
    }
}

pub(crate) async fn ingest_worker(
    db: crate::state::DatabaseConnection,
    default_project_id: Arc<str>,
    mut rx: mpsc::Receiver<BatchIngestRequest>,
    stats: Arc<IngestStats>,
) {
    const MAX_BATCHES: usize = 200;
    let window = Duration::from_millis(50);

    while let Some(first) = rx.recv().await {
        let mut batches = Vec::with_capacity(MAX_BATCHES);
        batches.push(first);

        let start = tokio::time::Instant::now();
        while batches.len() < MAX_BATCHES {
            let elapsed = start.elapsed();
            let remaining = match window.checked_sub(elapsed) {
                Some(r) if !r.is_zero() => r,
                _ => break,
            };

            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(p)) => batches.push(p),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        let item_count = batches
            .iter()
            .map(|b| b.trace.as_ref().map(|_| 1).unwrap_or(0) + b.observations.len())
            .sum::<usize>() as u64;

        let res = match &db {
            crate::state::DatabaseConnection::Postgres(pool) => {
                write_batches(pool, default_project_id.as_ref(), batches)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
            crate::state::DatabaseConnection::Memory(mem_db) => {
                write_batches_to_memory(mem_db, default_project_id.as_ref(), batches)
            }
        };

        match res {
            Ok(()) => {
                stats.record_batches_written(1);
                stats.record_items_written(item_count);
            }
            Err(err) => {
                stats.record_batches_failed(1);
                tracing::error!(error = ?err, "failed to write batch");
            }
        }
    }
}

fn resolve_trace(
    trace: TraceIngest,
    default_project_id: &str,
    now: DateTime<Utc>,
) -> ResolvedTrace {
    ResolvedTrace {
        id: trace.id,
        project_id: trace
            .projectId
            .unwrap_or_else(|| default_project_id.to_string()),
        environment: trace.environment.unwrap_or_else(|| "default".to_string()),
        timestamp: trace.timestamp.unwrap_or(now),
        name: trace.name,
        input: trace.input,
        output: trace.output,
        session_id: trace.session_id,
        release: trace.release,
        version: trace.version,
        user_id: trace.userId,
        metadata: trace.metadata,
        tags: trace.tags,
        public: trace.public.unwrap_or(false),
        external_id: trace.externalId,
        bookmarked: trace.bookmarked.unwrap_or(false),
        latency: trace.latency,
        total_cost: trace.totalCost,
    }
}

fn resolve_observation(obs: ObservationIngest, default_project_id: &str) -> ResolvedObservation {
    ResolvedObservation {
        id: obs.id,
        trace_id: obs.traceId,
        project_id: obs
            .projectId
            .unwrap_or_else(|| default_project_id.to_string()),
        environment: obs.environment.unwrap_or_else(|| "default".to_string()),
        obs_type: obs.r#type.unwrap_or_else(|| "GENERATION".to_string()),
        name: obs.name,
        start_time: obs.startTime,
        end_time: obs.endTime,
        completion_start_time: obs.completionStartTime,
        model: obs.model,
        model_parameters: obs.modelParameters,
        input: obs.input,
        output: obs.output,
        usage: obs.usage,
        level: obs.level,
        status_message: obs.statusMessage,
        parent_observation_id: obs.parentObservationId,
        prompt_id: obs.promptId,
        prompt_name: obs.promptName,
        prompt_version: obs.promptVersion,
        model_id: obs.modelId,
        input_price: obs.inputPrice,
        output_price: obs.outputPrice,
        total_price: obs.totalPrice,
        calculated_input_cost: obs.calculatedInputCost,
        calculated_output_cost: obs.calculatedOutputCost,
        calculated_total_cost: obs.calculatedTotalCost,
        latency: obs.latency,
        time_to_first_token: obs.timeToFirstToken,
        completion_tokens: obs.completionTokens,
        prompt_tokens: obs.promptTokens,
        total_tokens: obs.totalTokens,
        unit: obs.unit,
        metadata: obs.metadata,
    }
}

fn duration_secs(start: DateTime<Utc>, end: DateTime<Utc>) -> Option<f64> {
    let secs = (end - start).num_milliseconds() as f64 / 1000.0;
    if secs.is_finite() && secs >= 0.0 {
        Some(secs)
    } else {
        None
    }
}

/// Fill missing trace/observation scalars from span timing and root span names.
pub(crate) fn enrich_batch(batch: &mut BatchIngestRequest) {
    for obs in &mut batch.observations {
        if obs.latency.is_none() {
            if let (Some(start), Some(end)) = (obs.startTime, obs.endTime) {
                obs.latency = duration_secs(start, end);
            }
        }
    }

    let mut min_start: Option<DateTime<Utc>> = None;
    let mut max_end: Option<DateTime<Utc>> = None;
    let mut root_name: Option<String> = None;

    for obs in &batch.observations {
        if let Some(start) = obs.startTime {
            min_start = Some(min_start.map_or(start, |cur| cur.min(start)));
        }
        if let Some(end) = obs.endTime {
            max_end = Some(max_end.map_or(end, |cur| cur.max(end)));
        }
        if obs.parentObservationId.is_none() {
            if root_name.is_none() {
                root_name = obs.name.clone();
            }
        }
    }

    let Some(trace) = batch.trace.as_mut() else {
        return;
    };

    if trace.name.is_none() {
        trace.name = root_name.or_else(|| batch.observations.first().and_then(|o| o.name.clone()));
    }
    if trace.latency.is_none() {
        if let (Some(start), Some(end)) = (min_start, max_end) {
            trace.latency = duration_secs(start, end);
        }
    }
}

async fn write_batches(
    pool: &PgPool,
    default_project_id: &str,
    mut payloads: Vec<BatchIngestRequest>,
) -> Result<(), sqlx::Error> {
    if payloads.is_empty() {
        return Ok(());
    }

    for payload in &mut payloads {
        enrich_batch(payload);
    }

    let now = Utc::now();
    let mut traces: Vec<ResolvedTrace> = Vec::new();
    let mut observations: Vec<ResolvedObservation> = Vec::new();
    let mut placeholders: HashMap<Uuid, (String, String)> = HashMap::new();

    for payload in payloads {
        if let Some(trace) = payload.trace {
            traces.push(resolve_trace(trace, default_project_id, now));
        }
        for obs in payload.observations {
            let resolved = resolve_observation(obs, default_project_id);
            placeholders
                .entry(resolved.trace_id)
                .or_insert((resolved.project_id.clone(), resolved.environment.clone()));
            observations.push(resolved);
        }
    }

    let mut tx = pool.begin().await?;

    if !placeholders.is_empty() {
        let mut builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(
            "INSERT INTO traces (id, project_id, environment, timestamp, created_at, updated_at) ",
        );
        builder.push_values(
            placeholders.iter(),
            |mut b, (trace_id, (project_id, env))| {
                b.push_bind(*trace_id)
                    .push_bind(project_id.clone())
                    .push_bind(env.clone())
                    .push_bind(now)
                    .push_bind(now)
                    .push_bind(now);
            },
        );
        builder.push(" ON CONFLICT (id) DO NOTHING");
        builder.build().execute(&mut *tx).await?;
    }

    if !traces.is_empty() {
        let mut seen = HashSet::new();
        traces.retain(|trace| seen.insert(trace.id));

        let mut builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(
            r#"INSERT INTO traces (
  id, project_id, environment, timestamp, name, input, output, session_id, release, version, user_id,
  metadata, tags, public, external_id, bookmarked, latency, total_cost, created_at, updated_at
) "#,
        );
        builder.push_values(traces.iter(), |mut b, trace| {
            b.push_bind(trace.id)
                .push_bind(trace.project_id.clone())
                .push_bind(trace.environment.clone())
                .push_bind(trace.timestamp)
                .push_bind(trace.name.clone())
                .push_bind(trace.input.clone())
                .push_bind(trace.output.clone())
                .push_bind(trace.session_id.clone())
                .push_bind(trace.release.clone())
                .push_bind(trace.version.clone())
                .push_bind(trace.user_id.clone())
                .push_bind(trace.metadata.clone())
                .push_bind(trace.tags.clone())
                .push_bind(trace.public)
                .push_bind(trace.external_id.clone())
                .push_bind(trace.bookmarked)
                .push_bind(trace.latency)
                .push_bind(trace.total_cost)
                .push_bind(now)
                .push_bind(now);
        });
        builder.push(
            r#" ON CONFLICT (id) DO UPDATE SET
  project_id = EXCLUDED.project_id,
  environment = EXCLUDED.environment,
  timestamp = EXCLUDED.timestamp,
  name = EXCLUDED.name,
  input = EXCLUDED.input,
  output = EXCLUDED.output,
  session_id = EXCLUDED.session_id,
  release = EXCLUDED.release,
  version = EXCLUDED.version,
  user_id = EXCLUDED.user_id,
  metadata = EXCLUDED.metadata,
  tags = EXCLUDED.tags,
  public = EXCLUDED.public,
  external_id = EXCLUDED.external_id,
  bookmarked = EXCLUDED.bookmarked,
  latency = EXCLUDED.latency,
  total_cost = EXCLUDED.total_cost,
  updated_at = NOW()"#,
        );
        builder.build().execute(&mut *tx).await?;
    }

    if !observations.is_empty() {
        let mut seen = HashSet::new();
        observations.retain(|obs| seen.insert(obs.id));

        let mut builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(
            r#"INSERT INTO observations (
  id, trace_id, type, name, start_time, end_time, completion_start_time,
  model, model_parameters, input, output, usage, level, status_message,
  parent_observation_id, prompt_id, prompt_name, prompt_version, model_id,
  input_price, output_price, total_price,
  calculated_input_cost, calculated_output_cost, calculated_total_cost,
  latency, time_to_first_token,
  completion_tokens, prompt_tokens, total_tokens, unit,
  metadata, environment, project_id, created_at, updated_at
) "#,
        );
        builder.push_values(observations.iter(), |mut b, obs| {
            b.push_bind(obs.id)
                .push_bind(obs.trace_id)
                .push_bind(obs.obs_type.clone())
                .push_bind(obs.name.clone())
                .push_bind(obs.start_time)
                .push_bind(obs.end_time)
                .push_bind(obs.completion_start_time)
                .push_bind(obs.model.clone())
                .push_bind(obs.model_parameters.clone())
                .push_bind(obs.input.clone())
                .push_bind(obs.output.clone())
                .push_bind(obs.usage.clone())
                .push_bind(obs.level.clone())
                .push_bind(obs.status_message.clone())
                .push_bind(obs.parent_observation_id)
                .push_bind(obs.prompt_id.clone())
                .push_bind(obs.prompt_name.clone())
                .push_bind(obs.prompt_version.clone())
                .push_bind(obs.model_id.clone())
                .push_bind(obs.input_price)
                .push_bind(obs.output_price)
                .push_bind(obs.total_price)
                .push_bind(obs.calculated_input_cost)
                .push_bind(obs.calculated_output_cost)
                .push_bind(obs.calculated_total_cost)
                .push_bind(obs.latency)
                .push_bind(obs.time_to_first_token)
                .push_bind(obs.completion_tokens)
                .push_bind(obs.prompt_tokens)
                .push_bind(obs.total_tokens)
                .push_bind(obs.unit.clone())
                .push_bind(obs.metadata.clone())
                .push_bind(obs.environment.clone())
                .push_bind(obs.project_id.clone())
                .push_bind(now)
                .push_bind(now);
        });
        builder.push(
            r#" ON CONFLICT (id) DO UPDATE SET
  trace_id = EXCLUDED.trace_id,
  type = EXCLUDED.type,
  name = EXCLUDED.name,
  start_time = EXCLUDED.start_time,
  end_time = EXCLUDED.end_time,
  completion_start_time = EXCLUDED.completion_start_time,
  model = EXCLUDED.model,
  model_parameters = EXCLUDED.model_parameters,
  input = EXCLUDED.input,
  output = EXCLUDED.output,
  usage = EXCLUDED.usage,
  level = EXCLUDED.level,
  status_message = EXCLUDED.status_message,
  parent_observation_id = EXCLUDED.parent_observation_id,
  prompt_id = EXCLUDED.prompt_id,
  prompt_name = EXCLUDED.prompt_name,
  prompt_version = EXCLUDED.prompt_version,
  model_id = EXCLUDED.model_id,
  input_price = EXCLUDED.input_price,
  output_price = EXCLUDED.output_price,
  total_price = EXCLUDED.total_price,
  calculated_input_cost = EXCLUDED.calculated_input_cost,
  calculated_output_cost = EXCLUDED.calculated_output_cost,
  calculated_total_cost = EXCLUDED.calculated_total_cost,
  latency = EXCLUDED.latency,
  time_to_first_token = EXCLUDED.time_to_first_token,
  completion_tokens = EXCLUDED.completion_tokens,
  prompt_tokens = EXCLUDED.prompt_tokens,
  total_tokens = EXCLUDED.total_tokens,
  unit = EXCLUDED.unit,
  metadata = EXCLUDED.metadata,
  environment = EXCLUDED.environment,
  project_id = EXCLUDED.project_id,
  updated_at = NOW()"#,
        );
        builder.build().execute(&mut *tx).await?;
    }

    tx.commit().await?;
    Ok(())
}

fn write_batches_to_memory(
    mem_db: &crate::state::MemoryDb,
    default_project_id: &str,
    mut payloads: Vec<BatchIngestRequest>,
) -> Result<(), anyhow::Error> {
    for payload in &mut payloads {
        enrich_batch(payload);
    }

    let now = Utc::now();
    let mut resolved_traces = Vec::new();
    let mut resolved_obs = Vec::new();
    let mut placeholders = HashMap::new();

    for payload in payloads {
        if let Some(trace) = payload.trace {
            resolved_traces.push(resolve_trace(trace, default_project_id, now));
        }
        for obs in payload.observations {
            let resolved = resolve_observation(obs, default_project_id);
            placeholders
                .entry(resolved.trace_id)
                .or_insert((resolved.project_id.clone(), resolved.environment.clone()));
            resolved_obs.push(resolved);
        }
    }

    // 1. Insert placeholders
    for (trace_id, (project_id, env)) in placeholders {
        if !mem_db.traces.contains_key(&trace_id) {
            let row = crate::state::TraceRow {
                id: trace_id,
                project_id: project_id.clone(),
                environment: env.clone(),
                timestamp: now,
                name: None,
                input: None,
                output: None,
                session_id: None,
                release: None,
                version: None,
                user_id: None,
                metadata: None,
                tags: vec![],
                public: false,
                external_id: None,
                bookmarked: false,
                latency: None,
                total_cost: None,
                created_at: now,
                updated_at: now,
            };
            mem_db.traces.insert(trace_id, row);
        }
    }

    // 2. Upsert resolved traces
    for t in resolved_traces {
        let (created_at, updated_at) = if let Some(existing) = mem_db.traces.get(&t.id) {
            (existing.created_at, now)
        } else {
            (now, now)
        };
        let row = crate::state::TraceRow {
            id: t.id,
            project_id: t.project_id,
            environment: t.environment,
            timestamp: t.timestamp,
            name: t.name,
            input: t.input,
            output: t.output,
            session_id: t.session_id,
            release: t.release,
            version: t.version,
            user_id: t.user_id,
            metadata: t.metadata,
            tags: t.tags,
            public: t.public,
            external_id: t.external_id,
            bookmarked: t.bookmarked,
            latency: t.latency,
            total_cost: t.total_cost,
            created_at,
            updated_at,
        };
        mem_db.traces.insert(t.id, row);
    }

    // 3. Upsert resolved observations
    for o in resolved_obs {
        let (created_at, updated_at) = if let Some(existing) = mem_db.observations.get(&o.id) {
            (existing.created_at, now)
        } else {
            (now, now)
        };
        let row = crate::state::ObservationRow {
            id: o.id,
            trace_id: o.trace_id,
            r#type: o.obs_type,
            name: o.name,
            start_time: o.start_time,
            end_time: o.end_time,
            completion_start_time: o.completion_start_time,
            model: o.model,
            model_parameters: o.model_parameters,
            input: o.input,
            output: o.output,
            usage: o.usage,
            level: o.level,
            status_message: o.status_message,
            parent_observation_id: o.parent_observation_id,
            prompt_id: o.prompt_id,
            prompt_name: o.prompt_name,
            prompt_version: o.prompt_version,
            model_id: o.model_id,
            input_price: o.input_price,
            output_price: o.output_price,
            total_price: o.total_price,
            calculated_input_cost: o.calculated_input_cost,
            calculated_output_cost: o.calculated_output_cost,
            calculated_total_cost: o.calculated_total_cost,
            latency: o.latency,
            time_to_first_token: o.time_to_first_token,
            completion_tokens: o.completion_tokens,
            prompt_tokens: o.prompt_tokens,
            total_tokens: o.total_tokens,
            unit: o.unit,
            metadata: o.metadata,
            environment: o.environment,
            project_id: o.project_id,
            created_at,
            updated_at,
        };
        mem_db.observations.insert(o.id, row);
    }

    Ok(())
}
