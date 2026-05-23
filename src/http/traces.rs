use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value as JsonValue;
use sqlx::QueryBuilder;
use std::collections::HashSet;
use uuid::Uuid;

use crate::{
    http::{
        common::{PageMeta, PagedData},
        error::ApiError,
    },
    state::AppState,
};

#[derive(Debug, serde::Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct TraceListQuery {
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,

    #[serde(default, rename = "userId")]
    user_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,

    #[serde(default, rename = "fromTimestamp")]
    from_timestamp: Option<DateTime<Utc>>,
    #[serde(default, rename = "toTimestamp")]
    to_timestamp: Option<DateTime<Utc>>,

    #[serde(default, rename = "orderBy")]
    order_by: Option<String>,

    #[serde(default)]
    tags: Vec<String>,

    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    environment: Vec<String>,

    #[serde(default)]
    fields: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct TraceListRowCore {
    id: Uuid,
    project_id: String,
    timestamp: DateTime<Utc>,
    name: Option<String>,
    session_id: Option<String>,
    release: Option<String>,
    version: Option<String>,
    user_id: Option<String>,
    tags: Vec<String>,
    public: bool,
    external_id: Option<String>,
    bookmarked: bool,
    environment: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct TraceListRow {
    id: Uuid,
    project_id: String,
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
    environment: String,
    latency: Option<f64>,
    total_cost: Option<f64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    observations: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceListItem {
    id: Uuid,
    timestamp: DateTime<Utc>,
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<JsonValue>,
    session_id: Option<String>,
    release: Option<String>,
    version: Option<String>,
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<JsonValue>,
    tags: Vec<String>,
    public: bool,
    project_id: String,
    external_id: Option<String>,
    bookmarked: bool,
    environment: String,
    html_path: String,
    latency: Option<f64>,
    total_cost: Option<f64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    observations: Vec<String>,
    scores: Vec<String>,
}

#[derive(Clone, Copy)]
struct TraceFieldsMask {
    io: bool,
    scores: bool,
    observations: bool,
    metrics: bool,
}

fn parse_trace_fields(fields: Option<&str>) -> TraceFieldsMask {
    let Some(fields) = fields else {
        return TraceFieldsMask {
            io: true,
            scores: true,
            observations: true,
            metrics: true,
        };
    };

    let set: HashSet<&str> = fields
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    TraceFieldsMask {
        io: set.contains("io"),
        scores: set.contains("scores"),
        observations: set.contains("observations"),
        metrics: set.contains("metrics"),
    }
}

fn apply_trace_filters(builder: &mut QueryBuilder<'_, sqlx::Postgres>, q: &TraceListQuery) {
    if let Some(user_id) = &q.user_id {
        builder.push(" AND t.user_id = ");
        builder.push_bind(user_id.clone());
    }
    if let Some(name) = &q.name {
        builder.push(" AND t.name = ");
        builder.push_bind(name.clone());
    }
    if let Some(session_id) = &q.session_id {
        builder.push(" AND t.session_id = ");
        builder.push_bind(session_id.clone());
    }
    if let Some(from_ts) = &q.from_timestamp {
        builder.push(" AND t.timestamp >= ");
        builder.push_bind(*from_ts);
    }
    if let Some(to_ts) = &q.to_timestamp {
        builder.push(" AND t.timestamp <= ");
        builder.push_bind(*to_ts);
    }
    if !q.tags.is_empty() {
        builder.push(" AND t.tags @> ");
        builder.push_bind(q.tags.clone());
    }

    if let Some(version) = &q.version {
        builder.push(" AND t.version = ");
        builder.push_bind(version.clone());
    }
    if let Some(release) = &q.release {
        builder.push(" AND t.release = ");
        builder.push_bind(release.clone());
    }
    if !q.environment.is_empty() {
        builder.push(" AND t.environment = ANY(");
        builder.push_bind(q.environment.clone());
        builder.push(")");
    }
}

fn parse_order_by(order_by: Option<&str>) -> Result<(&'static str, bool), ApiError> {
    let s = order_by.unwrap_or("timestamp.desc").trim();
    let (col, dir) = s.split_once('.').unwrap_or((s, "desc"));
    let (col, default_desc) = match col {
        "id" => ("t.id", true),
        "timestamp" => ("t.timestamp", true),
        "name" => ("t.name", false),
        "userId" | "user_id" => ("t.user_id", false),
        "release" => ("t.release", false),
        "version" => ("t.version", false),
        "public" => ("t.public", true),
        "bookmarked" => ("t.bookmarked", true),
        "sessionId" | "session_id" => ("t.session_id", false),
        "latency" => ("t.latency", true),
        "totalCost" | "total_cost" => ("t.total_cost", true),
        _ => return Err(ApiError::BadRequest("invalid order_by".into())),
    };
    let desc = match dir {
        "desc" => true,
        "asc" => false,
        _ => default_desc,
    };
    Ok((col, desc))
}

pub(crate) async fn get_traces(
    State(state): State<AppState>,
    Query(q): Query<TraceListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let page = q.page.unwrap_or(1).max(1);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * limit;

    let fields = parse_trace_fields(q.fields.as_deref());
    let (order_column, order_desc) = parse_order_by(q.order_by.as_deref())?;
    let project_id = state.default_project_id.to_string();

    match &state.db {
        crate::state::DatabaseConnection::Postgres(pool) => {
            let mut count_builder: QueryBuilder<'_, sqlx::Postgres> =
                QueryBuilder::new("SELECT COUNT(*)::BIGINT AS cnt FROM traces t WHERE 1=1");
            count_builder.push(" AND t.project_id = ");
            count_builder.push_bind(project_id.clone());
            apply_trace_filters(&mut count_builder, &q);

            let total_items: i64 = count_builder
                .build_query_scalar()
                .fetch_one(pool)
                .await?;

            let total_pages = if total_items == 0 {
                0
            } else {
                (total_items + limit - 1) / limit
            };

            if !fields.io && !fields.observations && !fields.metrics {
                let mut builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(
                    r#"
SELECT
  t.id,
  t.project_id,
  t.timestamp,
  t.name,
  t.session_id,
  t.release,
  t.version,
  t.user_id,
  t.tags,
  t.public,
  t.external_id,
  t.bookmarked,
  t.environment,
  t.created_at,
  t.updated_at
FROM (
  SELECT *
  FROM traces t
  WHERE 1=1
                    "#,
                );

                builder.push(" AND t.project_id = ");
                builder.push_bind(project_id.clone());
                apply_trace_filters(&mut builder, &q);
                builder.push(" ORDER BY ");
                builder.push(order_column);
                builder.push(if order_desc { " DESC" } else { " ASC" });
                builder.push(" LIMIT ");
                builder.push_bind(limit);
                builder.push(" OFFSET ");
                builder.push_bind(offset);
                builder.push(") t");

                let rows: Vec<TraceListRowCore> = builder.build_query_as().fetch_all(pool).await?;

                let items = rows
                    .into_iter()
                    .map(|r| TraceListItem {
                        html_path: format!("/project/{}/traces/{}", r.project_id, r.id),
                        id: r.id,
                        timestamp: r.timestamp,
                        name: r.name,
                        input: None,
                        output: None,
                        session_id: r.session_id,
                        release: r.release,
                        version: r.version,
                        user_id: r.user_id,
                        metadata: None,
                        tags: r.tags,
                        public: r.public,
                        project_id: r.project_id,
                        external_id: r.external_id,
                        bookmarked: r.bookmarked,
                        environment: r.environment,
                        latency: Some(-1.0),
                        total_cost: Some(-1.0),
                        created_at: r.created_at,
                        updated_at: r.updated_at,
                        observations: vec![],
                        scores: vec![],
                    })
                    .collect::<Vec<_>>();

                return Ok((
                    StatusCode::OK,
                    Json(PagedData {
                        data: items,
                        meta: PageMeta {
                            page,
                            limit,
                            totalItems: total_items,
                            totalPages: total_pages,
                        },
                    }),
                ));
            }

            let mut select_cols = String::from(
                r#"
SELECT
  t.id,
  t.project_id,
  t.timestamp,
  t.name,
                "#,
            );
            if fields.io {
                select_cols.push_str("  t.input,\n  t.output,\n");
            } else {
                select_cols.push_str("  NULL::jsonb AS input,\n  NULL::jsonb AS output,\n");
            }
            select_cols.push_str(
                r#"  t.session_id,
  t.release,
  t.version,
  t.user_id,
"#,
            );
            if fields.io {
                select_cols.push_str("  t.metadata,\n");
            } else {
                select_cols.push_str("  NULL::jsonb AS metadata,\n");
            }
            select_cols.push_str(
                r#"  t.tags,
  t.public,
  t.external_id,
  t.bookmarked,
  t.environment,
"#,
            );
            if fields.metrics {
                select_cols.push_str("  t.latency,\n  t.total_cost,\n");
            } else {
                select_cols.push_str(
                    "  NULL::double precision AS latency,\n  NULL::double precision AS total_cost,\n",
                );
            }
            select_cols.push_str("  t.created_at,\n  t.updated_at,\n");
            if fields.observations {
                select_cols.push_str(
                    "  COALESCE((SELECT array_agg(id) FROM observations WHERE trace_id = t.id), '{}') AS observations\n",
                );
            } else {
                select_cols.push_str("  '{}'::uuid[] AS observations\n");
            }
            select_cols.push_str(
                r#"FROM (
  SELECT *
  FROM traces t
  WHERE 1=1
                "#,
            );

            let mut builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(&select_cols);

            builder.push(" AND t.project_id = ");
            builder.push_bind(&project_id);

            apply_trace_filters(&mut builder, &q);

            builder.push(" ORDER BY ");
            builder.push(order_column);
            builder.push(if order_desc { " DESC" } else { " ASC" });
            builder.push(" LIMIT ");
            builder.push_bind(limit);
            builder.push(" OFFSET ");
            builder.push_bind(offset);

            builder.push(") t");

            let rows: Vec<TraceListRow> = builder.build_query_as().fetch_all(pool).await?;

            let items = rows
                .into_iter()
                .map(|r| {
                    let observations = if fields.observations {
                        r.observations
                            .into_iter()
                            .map(|id| id.to_string())
                            .collect()
                    } else {
                        vec![]
                    };
                    let scores = if fields.scores {
                        Vec::new()
                    } else {
                        Vec::with_capacity(0)
                    };

                    let latency = if fields.metrics {
                        r.latency
                    } else {
                        Some(-1.0)
                    };
                    let total_cost = if fields.metrics {
                        r.total_cost
                    } else {
                        Some(-1.0)
                    };

                    TraceListItem {
                        html_path: format!("/project/{}/traces/{}", r.project_id, r.id),
                        id: r.id,
                        timestamp: r.timestamp,
                        name: r.name,
                        input: if fields.io {
                            Some(r.input.unwrap_or(JsonValue::Null))
                        } else {
                            None
                        },
                        output: if fields.io {
                            Some(r.output.unwrap_or(JsonValue::Null))
                        } else {
                            None
                        },
                        session_id: r.session_id,
                        release: r.release,
                        version: r.version,
                        user_id: r.user_id,
                        metadata: if fields.io {
                            Some(r.metadata.unwrap_or(JsonValue::Null))
                        } else {
                            None
                        },
                        tags: r.tags,
                        public: r.public,
                        project_id: r.project_id,
                        external_id: r.external_id,
                        bookmarked: r.bookmarked,
                        environment: r.environment,
                        latency,
                        total_cost,
                        created_at: r.created_at,
                        updated_at: r.updated_at,
                        observations,
                        scores,
                    }
                })
                .collect::<Vec<_>>();

            Ok((
                StatusCode::OK,
                Json(PagedData {
                    data: items,
                    meta: PageMeta {
                        page,
                        limit,
                        totalItems: total_items,
                        totalPages: total_pages,
                    },
                }),
            ))
        }
        crate::state::DatabaseConnection::Memory(mem_db) => {
            let mut filtered: Vec<crate::state::TraceRow> = mem_db
                .traces
                .iter()
                .map(|item| item.value().clone())
                .filter(|t| {
                    if t.project_id != project_id {
                        return false;
                    }
                    if let Some(ref u_id) = q.user_id {
                        if t.user_id.as_ref() != Some(u_id) {
                            return false;
                        }
                    }
                    if let Some(ref name) = q.name {
                        if t.name.as_ref() != Some(name) {
                            return false;
                        }
                    }
                    if let Some(ref s_id) = q.session_id {
                        if t.session_id.as_ref() != Some(s_id) {
                            return false;
                        }
                    }
                    if let Some(ref from_ts) = q.from_timestamp {
                        if t.timestamp < *from_ts {
                            return false;
                        }
                    }
                    if let Some(ref to_ts) = q.to_timestamp {
                        if t.timestamp > *to_ts {
                            return false;
                        }
                    }
                    if !q.tags.is_empty() {
                        if !q.tags.iter().all(|tag| t.tags.contains(tag)) {
                            return false;
                        }
                    }
                    if let Some(ref version) = q.version {
                        if t.version.as_ref() != Some(version) {
                            return false;
                        }
                    }
                    if let Some(ref release) = q.release {
                        if t.release.as_ref() != Some(release) {
                            return false;
                        }
                    }
                    if !q.environment.is_empty() {
                        if !q.environment.contains(&t.environment) {
                            return false;
                        }
                    }
                    true
                })
                .collect();

            filtered.sort_by(|a, b| {
                let ord = match order_column {
                    "t.id" => a.id.cmp(&b.id),
                    "t.timestamp" => a.timestamp.cmp(&b.timestamp),
                    "t.name" => a.name.cmp(&b.name),
                    "t.user_id" => a.user_id.cmp(&b.user_id),
                    "t.release" => a.release.cmp(&b.release),
                    "t.version" => a.version.cmp(&b.version),
                    "t.public" => a.public.cmp(&b.public),
                    "t.bookmarked" => a.bookmarked.cmp(&b.bookmarked),
                    "t.session_id" => a.session_id.cmp(&b.session_id),
                    "t.latency" => {
                        let a_lat = a.latency.unwrap_or(0.0);
                        let b_lat = b.latency.unwrap_or(0.0);
                        a_lat.partial_cmp(&b_lat).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    "t.total_cost" => {
                        let a_cost = a.total_cost.unwrap_or(0.0);
                        let b_cost = b.total_cost.unwrap_or(0.0);
                        a_cost.partial_cmp(&b_cost).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    _ => a.timestamp.cmp(&b.timestamp),
                };
                if order_desc {
                    ord.reverse()
                } else {
                    ord
                }
            });

            let total_items = filtered.len() as i64;
            let total_pages = if total_items == 0 {
                0
            } else {
                (total_items + limit - 1) / limit
            };
            let paged: Vec<crate::state::TraceRow> = filtered
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect();

            let mut trace_obs_ids: std::collections::HashMap<Uuid, Vec<Uuid>> = std::collections::HashMap::new();
            if fields.observations {
                for entry in mem_db.observations.iter() {
                    let obs = entry.value();
                    trace_obs_ids.entry(obs.trace_id).or_default().push(obs.id);
                }
            }

            let items: Vec<TraceListItem> = paged
                .into_iter()
                .map(|r| {
                    let observations = if fields.observations {
                        trace_obs_ids
                            .get(&r.id)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|id| id.to_string())
                            .collect()
                    } else {
                        vec![]
                    };

                    TraceListItem {
                        html_path: format!("/project/{}/traces/{}", r.project_id, r.id),
                        id: r.id,
                        timestamp: r.timestamp,
                        name: r.name,
                        input: if fields.io {
                            Some(r.input.unwrap_or(JsonValue::Null))
                        } else {
                            None
                        },
                        output: if fields.io {
                            Some(r.output.unwrap_or(JsonValue::Null))
                        } else {
                            None
                        },
                        session_id: r.session_id,
                        release: r.release,
                        version: r.version,
                        user_id: r.user_id,
                        metadata: if fields.io {
                            Some(r.metadata.unwrap_or(JsonValue::Null))
                        } else {
                            None
                        },
                        tags: r.tags,
                        public: r.public,
                        project_id: r.project_id,
                        external_id: r.external_id,
                        bookmarked: r.bookmarked,
                        environment: r.environment,
                        latency: if fields.metrics {
                            r.latency
                        } else {
                            Some(-1.0)
                        },
                        total_cost: if fields.metrics {
                            r.total_cost
                        } else {
                            Some(-1.0)
                        },
                        created_at: r.created_at,
                        updated_at: r.updated_at,
                        observations,
                        scores: vec![],
                    }
                })
                .collect();

            Ok((
                StatusCode::OK,
                Json(PagedData {
                    data: items,
                    meta: PageMeta {
                        page,
                        limit,
                        totalItems: total_items,
                        totalPages: total_pages,
                    },
                }),
            ))
        }
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[allow(dead_code)]
struct ObservationRow {
    id: Uuid,
    trace_id: Uuid,
    r#type: String,
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
    environment: String,
    project_id: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicUsage {
    input: i64,
    output: i64,
    total: i64,
    unit: Option<String>,
    input_cost: Option<f64>,
    output_cost: Option<f64>,
    total_cost: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObservationsViewDto {
    id: Uuid,
    trace_id: Option<Uuid>,
    r#type: String,
    name: Option<String>,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
    completion_start_time: Option<DateTime<Utc>>,
    model: Option<String>,
    model_parameters: JsonValue,
    input: JsonValue,
    version: Option<String>,
    metadata: JsonValue,
    output: JsonValue,
    usage: PublicUsage,
    level: String,
    status_message: Option<String>,
    parent_observation_id: Option<Uuid>,
    prompt_id: Option<String>,
    prompt_name: Option<String>,
    prompt_version: Option<i64>,
    model_id: Option<String>,
    input_price: Option<f64>,
    output_price: Option<f64>,
    total_price: Option<f64>,
    calculated_input_cost: Option<f64>,
    calculated_output_cost: Option<f64>,
    calculated_total_cost: Option<f64>,
    latency: Option<f64>,
    time_to_first_token: Option<f64>,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    usage_details: JsonValue,
    cost_details: JsonValue,
    environment: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct TraceRow {
    id: Uuid,
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
    environment: String,
    latency: Option<f64>,
    total_cost: Option<f64>,
    external_id: Option<String>,
    bookmarked: bool,
    project_id: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceDetailDto {
    id: Uuid,
    timestamp: DateTime<Utc>,
    name: Option<String>,
    input: JsonValue,
    output: JsonValue,
    session_id: Option<String>,
    release: Option<String>,
    version: Option<String>,
    user_id: Option<String>,
    metadata: JsonValue,
    tags: Vec<String>,
    public: bool,
    project_id: String,
    external_id: Option<String>,
    bookmarked: bool,
    environment: String,
    html_path: String,
    latency: Option<f64>,
    total_cost: Option<f64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    observations: Vec<ObservationsViewDto>,
    scores: Vec<ScoreV1Dto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScoreV1Dto {
    #[serde(rename = "dataType")]
    data_type: String,
    id: String,
    trace_id: String,
    name: String,
    source: String,
    observation_id: Option<String>,
    timestamp: DateTime<Utc>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    author_user_id: Option<String>,
    comment: Option<String>,
    metadata: JsonValue,
    config_id: Option<String>,
    queue_id: Option<String>,
    environment: String,
    value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    string_value: Option<String>,
}

pub(crate) async fn get_trace(
    State(state): State<AppState>,
    Path(trace_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = state.default_project_id.as_ref();

    match &state.db {
        crate::state::DatabaseConnection::Postgres(pool) => {
            let trace: Option<TraceRow> = sqlx::query_as(
                r#"
SELECT
  id,
  timestamp,
  name,
  input,
  output,
  session_id,
  release,
  version,
  user_id,
  metadata,
  tags,
  public,
  environment,
  latency,
  total_cost,
  external_id,
  bookmarked,
  project_id,
  created_at,
  updated_at
FROM traces
WHERE id = $1 AND project_id = $2
                 "#,
            )
            .bind(trace_id)
            .bind(project_id)
            .fetch_optional(pool)
            .await?;

            let Some(trace) = trace else {
                return Err(ApiError::NotFound);
            };

            let observations: Vec<ObservationRow> = sqlx::query_as(
                r#"
SELECT
  id,
  trace_id,
  type as "type",
  name,
  start_time,
  end_time,
  completion_start_time,
  model,
  model_parameters,
  input,
  output,
  usage,
  level,
  status_message,
  parent_observation_id,
  prompt_id,
  prompt_name,
  prompt_version,
  model_id,
  input_price,
  output_price,
  total_price,
  calculated_input_cost,
  calculated_output_cost,
  calculated_total_cost,
  latency,
  time_to_first_token,
  completion_tokens,
  prompt_tokens,
  total_tokens,
  unit,
  metadata,
  environment,
  project_id,
  created_at,
  updated_at
FROM observations
WHERE trace_id = $1 AND project_id = $2
ORDER BY start_time NULLS LAST, created_at
                 "#,
            )
            .bind(trace_id)
            .bind(project_id)
            .fetch_all(pool)
            .await?;

            let obs_dtos = observations
                .into_iter()
                .map(|o| {
                    let prompt_tokens = o.prompt_tokens.unwrap_or(0);
                    let completion_tokens = o.completion_tokens.unwrap_or(0);
                    let total_tokens = o.total_tokens.unwrap_or(0);
                    let calculated_input_cost = o.calculated_input_cost.unwrap_or(0.0);
                    let calculated_output_cost = o.calculated_output_cost.unwrap_or(0.0);
                    let calculated_total_cost = o.calculated_total_cost.unwrap_or(0.0);

                    ObservationsViewDto {
                        version: None,
                        id: o.id,
                        trace_id: Some(o.trace_id),
                        r#type: o.r#type,
                        name: o.name,
                        start_time: o.start_time.unwrap_or(o.created_at),
                        end_time: o.end_time,
                        completion_start_time: o.completion_start_time,
                        model: o.model,
                        model_parameters: o.model_parameters.unwrap_or_else(|| serde_json::json!({})),
                        input: o.input.unwrap_or(JsonValue::Null),
                        metadata: o.metadata.unwrap_or(JsonValue::Null),
                        output: o.output.unwrap_or(JsonValue::Null),
                        usage: PublicUsage {
                            input: prompt_tokens,
                            output: completion_tokens,
                            total: total_tokens,
                            unit: o.unit.clone(),
                            input_cost: o.calculated_input_cost,
                            output_cost: o.calculated_output_cost,
                            total_cost: o.calculated_total_cost,
                        },
                        usage_details: serde_json::json!({
                            "input": prompt_tokens,
                            "output": completion_tokens,
                            "total": total_tokens
                        }),
                        cost_details: serde_json::json!({
                            "input": calculated_input_cost,
                            "output": calculated_output_cost,
                            "total": calculated_total_cost
                        }),
                        level: o.level.unwrap_or_else(|| "DEFAULT".to_string()),
                        status_message: o.status_message,
                        parent_observation_id: o.parent_observation_id,
                        prompt_id: o.prompt_id,
                        prompt_name: o.prompt_name,
                        prompt_version: o
                            .prompt_version
                            .as_deref()
                            .and_then(|s| s.parse::<i64>().ok()),
                        model_id: o.model_id,
                        input_price: o.input_price,
                        output_price: o.output_price,
                        total_price: o.total_price,
                        calculated_input_cost: o.calculated_input_cost,
                        calculated_output_cost: o.calculated_output_cost,
                        calculated_total_cost: o.calculated_total_cost,
                        latency: o.latency,
                        time_to_first_token: o.time_to_first_token,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        environment: o.environment,
                    }
                })
                .collect::<Vec<_>>();

            let dto = TraceDetailDto {
                html_path: format!("/project/{}/traces/{}", trace.project_id, trace.id),
                scores: vec![],
                id: trace.id,
                timestamp: trace.timestamp,
                name: trace.name,
                input: trace.input.unwrap_or(JsonValue::Null),
                output: trace.output.unwrap_or(JsonValue::Null),
                session_id: trace.session_id,
                release: trace.release,
                version: trace.version,
                user_id: trace.user_id,
                metadata: trace.metadata.unwrap_or(JsonValue::Null),
                tags: trace.tags,
                public: trace.public,
                project_id: trace.project_id,
                external_id: trace.external_id,
                bookmarked: trace.bookmarked,
                environment: trace.environment,
                latency: trace.latency,
                total_cost: trace.total_cost,
                created_at: trace.created_at,
                updated_at: trace.updated_at,
                observations: obs_dtos,
            };

            Ok((StatusCode::OK, Json(dto)))
        }
        crate::state::DatabaseConnection::Memory(mem_db) => {
            let trace = mem_db
                .traces
                .get(&trace_id)
                .map(|item| item.value().clone())
                .filter(|t| t.project_id == project_id);

            let Some(trace) = trace else {
                return Err(ApiError::NotFound);
            };

            let mut observations: Vec<crate::state::ObservationRow> = mem_db
                .observations
                .iter()
                .map(|item| item.value().clone())
                .filter(|o| o.trace_id == trace_id && o.project_id == project_id)
                .collect();

            observations.sort_by(|a, b| {
                let a_time = a.start_time.unwrap_or(a.created_at);
                let b_time = b.start_time.unwrap_or(b.created_at);
                a_time.cmp(&b_time).then_with(|| a.created_at.cmp(&b.created_at))
            });

            let obs_dtos = observations
                .into_iter()
                .map(|o| {
                    let prompt_tokens = o.prompt_tokens.unwrap_or(0);
                    let completion_tokens = o.completion_tokens.unwrap_or(0);
                    let total_tokens = o.total_tokens.unwrap_or(0);
                    let calculated_input_cost = o.calculated_input_cost.unwrap_or(0.0);
                    let calculated_output_cost = o.calculated_output_cost.unwrap_or(0.0);
                    let calculated_total_cost = o.calculated_total_cost.unwrap_or(0.0);

                    ObservationsViewDto {
                        version: None,
                        id: o.id,
                        trace_id: Some(o.trace_id),
                        r#type: o.r#type,
                        name: o.name,
                        start_time: o.start_time.unwrap_or(o.created_at),
                        end_time: o.end_time,
                        completion_start_time: o.completion_start_time,
                        model: o.model,
                        model_parameters: o.model_parameters.unwrap_or_else(|| serde_json::json!({})),
                        input: o.input.unwrap_or(JsonValue::Null),
                        metadata: o.metadata.unwrap_or(JsonValue::Null),
                        output: o.output.unwrap_or(JsonValue::Null),
                        usage: PublicUsage {
                            input: prompt_tokens,
                            output: completion_tokens,
                            total: total_tokens,
                            unit: o.unit.clone(),
                            input_cost: o.calculated_input_cost,
                            output_cost: o.calculated_output_cost,
                            total_cost: o.calculated_total_cost,
                        },
                        usage_details: serde_json::json!({
                            "input": prompt_tokens,
                            "output": completion_tokens,
                            "total": total_tokens
                        }),
                        cost_details: serde_json::json!({
                            "input": calculated_input_cost,
                            "output": calculated_output_cost,
                            "total": calculated_total_cost
                        }),
                        level: o.level.unwrap_or_else(|| "DEFAULT".to_string()),
                        status_message: o.status_message,
                        parent_observation_id: o.parent_observation_id,
                        prompt_id: o.prompt_id,
                        prompt_name: o.prompt_name,
                        prompt_version: o
                            .prompt_version
                            .as_deref()
                            .and_then(|s| s.parse::<i64>().ok()),
                        model_id: o.model_id,
                        input_price: o.input_price,
                        output_price: o.output_price,
                        total_price: o.total_price,
                        calculated_input_cost: o.calculated_input_cost,
                        calculated_output_cost: o.calculated_output_cost,
                        calculated_total_cost: o.calculated_total_cost,
                        latency: o.latency,
                        time_to_first_token: o.time_to_first_token,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        environment: o.environment,
                    }
                })
                .collect::<Vec<_>>();

            let dto = TraceDetailDto {
                html_path: format!("/project/{}/traces/{}", trace.project_id, trace.id),
                scores: vec![],
                id: trace.id,
                timestamp: trace.timestamp,
                name: trace.name,
                input: trace.input.unwrap_or(JsonValue::Null),
                output: trace.output.unwrap_or(JsonValue::Null),
                session_id: trace.session_id,
                release: trace.release,
                version: trace.version,
                user_id: trace.user_id,
                metadata: trace.metadata.unwrap_or(JsonValue::Null),
                tags: trace.tags,
                public: trace.public,
                project_id: trace.project_id,
                external_id: trace.external_id,
                bookmarked: trace.bookmarked,
                environment: trace.environment,
                latency: trace.latency,
                total_cost: trace.total_cost,
                created_at: trace.created_at,
                updated_at: trace.updated_at,
                observations: obs_dtos,
            };

            Ok((StatusCode::OK, Json(dto)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trace_fields_defaults_to_all() {
        let mask = parse_trace_fields(None);
        assert!(mask.io);
        assert!(mask.scores);
        assert!(mask.observations);
        assert!(mask.metrics);
    }

    #[test]
    fn parse_trace_fields_core_excludes_heavy_fields() {
        let mask = parse_trace_fields(Some("core"));
        assert!(!mask.io);
        assert!(!mask.scores);
        assert!(!mask.observations);
        assert!(!mask.metrics);
    }

    #[test]
    fn parse_trace_fields_partial_selection() {
        let mask = parse_trace_fields(Some("io,metrics"));
        assert!(mask.io);
        assert!(!mask.scores);
        assert!(!mask.observations);
        assert!(mask.metrics);
    }

    #[test]
    fn parse_order_by_accepts_timestamp_desc() {
        let (col, desc) = parse_order_by(Some("timestamp.desc")).unwrap();
        assert_eq!(col, "t.timestamp");
        assert!(desc);
    }

    #[test]
    fn parse_order_by_rejects_unknown_column() {
        assert!(parse_order_by(Some("unknown.asc")).is_err());
    }
}
