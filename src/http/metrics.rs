use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::QueryBuilder;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::{sync::mpsc, time::Duration};

use crate::{
    http::{
        common::{ApiResponse, PageMeta, PagedData},
        error::ApiError,
    },
    state::AppState,
};

type MetricBucketPoints = BTreeMap<DateTime<Utc>, Vec<(f64, DateTime<Utc>)>>;
type MetricGroupBucket = (JsonValue, MetricBucketPoints);
type DailyTraceGroup = (
    Vec<crate::state::TraceRow>,
    Vec<crate::state::ObservationRow>,
);
type DailyModelGroup = (
    i64,
    i64,
    i64,
    std::collections::HashSet<uuid::Uuid>,
    i64,
    f64,
);

#[derive(Debug, Deserialize)]
pub(crate) struct MetricsBatchRequest {
    pub metrics: Vec<MetricPointIngest>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MetricPointIngest {
    pub name: String,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct MetricsQuery {
    name: String,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    labels: Option<String>,
    step: Option<String>,
    agg: Option<String>,
    group_by: Option<String>,
}

#[derive(Debug, Serialize)]
struct MetricValuePoint {
    timestamp: String,
    value: f64,
}

#[derive(Debug, Serialize)]
struct MetricsSeries {
    labels: JsonValue,
    values: Vec<MetricValuePoint>,
}

#[derive(Debug, Serialize)]
struct MetricsQueryMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_ts: Option<String>,
    series_count: usize,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct MetricsQueryResponse {
    data: Vec<MetricsSeries>,
    meta: MetricsQueryMeta,
}

pub(crate) async fn post_metrics_batch(
    State(state): State<AppState>,
    Json(payload): Json<MetricsBatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    match state.metrics_tx.try_send(payload) {
        Ok(()) => Ok((
            StatusCode::OK,
            Json(ApiResponse::<serde_json::Value> {
                message: "Request Successful.".to_string(),
                code: None,
                data: None,
            }),
        )),
        Err(mpsc::error::TrySendError::Full(_)) => Err(ApiError::TooManyRequests),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(ApiError::ServiceUnavailable),
    }
}

fn parse_step_seconds(step: Option<&str>) -> Result<i64, ApiError> {
    let step = step.unwrap_or("1m").trim();
    let secs = match step {
        "1m" => 60,
        "5m" => 300,
        "1h" => 3600,
        "1d" => 86400,
        _ => {
            return Err(ApiError::BadRequest(
                "invalid step, must be one of: 1m, 5m, 1h, 1d".to_string(),
            ))
        }
    };
    Ok(secs)
}

fn parse_agg(agg: Option<&str>) -> Result<&'static str, ApiError> {
    let agg = agg.unwrap_or("avg").trim().to_ascii_lowercase();
    match agg.as_str() {
        "avg" => Ok("avg"),
        "max" => Ok("max"),
        "min" => Ok("min"),
        "sum" => Ok("sum"),
        "last" => Ok("last"),
        "count" => Ok("count"),
        "p50" => Ok("p50"),
        "p90" => Ok("p90"),
        "p99" => Ok("p99"),
        _ => Err(ApiError::BadRequest(
            "invalid agg, must be one of: avg, max, min, sum, last, count, p50, p90, p99"
                .to_string(),
        )),
    }
}

fn parse_group_by_keys(group_by: Option<&str>) -> Vec<String> {
    group_by
        .into_iter()
        .flat_map(|group_by| group_by.split(','))
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect()
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn labels_and_group_key_from_json(
    labels: &JsonValue,
    group_by_keys: &[String],
) -> (JsonValue, String) {
    if group_by_keys.is_empty() {
        return (labels.clone(), labels.to_string());
    }

    let mut pairs = Vec::with_capacity(group_by_keys.len());
    let mut map = serde_json::Map::with_capacity(group_by_keys.len());
    let source = labels.as_object();
    for key in group_by_keys {
        let value = source
            .and_then(|m| m.get(key))
            .cloned()
            .unwrap_or(JsonValue::Null);
        pairs.push((key.clone(), value.clone()));
        map.insert(key.clone(), value);
    }

    let labels_json = JsonValue::Object(map);
    let key = serde_json::to_string(&pairs).unwrap_or_else(|_| labels_json.to_string());
    (labels_json, key)
}

fn group_key_from_json(labels: &JsonValue, group_by_keys: &[String]) -> String {
    if group_by_keys.is_empty() {
        return labels.to_string();
    }

    let mut pairs = Vec::with_capacity(group_by_keys.len());
    let source = labels.as_object();
    for key in group_by_keys {
        let value = source
            .and_then(|m| m.get(key))
            .cloned()
            .unwrap_or(JsonValue::Null);
        pairs.push((key.clone(), value));
    }

    serde_json::to_string(&pairs).unwrap_or_else(|_| labels.to_string())
}

pub(crate) async fn get_metrics_names(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = state.default_project_id.as_ref();

    let names: Vec<String> = match &state.db {
        crate::state::DatabaseConnection::Postgres(pool) => {
            sqlx::query_scalar(
                r#"
SELECT DISTINCT name
FROM metrics
WHERE project_id = $1 AND environment = 'default'
ORDER BY name
                "#,
            )
            .bind(project_id)
            .fetch_all(pool)
            .await?
        }
        crate::state::DatabaseConnection::Memory(mem_db) => {
            let metrics_guard = mem_db
                .metrics
                .lock()
                .map_err(|e| ApiError::Internal(format!("Mutex lock error: {e}")))?;
            let mut unique_names: Vec<String> = metrics_guard
                .iter()
                .filter(|m| m.project_id == project_id && m.environment == "default")
                .map(|m| m.name.clone())
                .collect();
            unique_names.sort();
            unique_names.dedup();
            unique_names
        }
    };

    Ok((StatusCode::OK, Json(serde_json::json!({ "data": names }))))
}

pub(crate) async fn get_metrics_query(
    State(state): State<AppState>,
    Query(q): Query<MetricsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let now = Utc::now();
    let to_ts = q.to.unwrap_or(now);
    let from_ts = q.from.unwrap_or_else(|| to_ts - chrono::Duration::hours(1));

    if from_ts > to_ts {
        return Err(ApiError::BadRequest("from must be <= to".to_string()));
    }

    let step_seconds = parse_step_seconds(q.step.as_deref())?;
    let agg = parse_agg(q.agg.as_deref())?;

    let labels_filter: Option<JsonValue> = match q.labels.as_deref() {
        Some(s) if !s.trim().is_empty() => Some(
            serde_json::from_str::<JsonValue>(s)
                .map_err(|e| ApiError::BadRequest(format!("invalid labels json: {e}")))?,
        ),
        _ => None,
    };
    let group_by_keys = parse_group_by_keys(q.group_by.as_deref());

    let project_id = state.default_project_id.as_ref();

    let mut series_map: BTreeMap<String, MetricsSeries> = BTreeMap::new();
    let mut points_truncated = false;
    let mut series_truncated = false;
    let mut latest_bucket: Option<DateTime<Utc>> = None;

    match &state.db {
        crate::state::DatabaseConnection::Postgres(pool) => {
            let agg_expr = match agg {
                "avg" => "AVG(value)::DOUBLE PRECISION",
                "max" => "MAX(value)::DOUBLE PRECISION",
                "min" => "MIN(value)::DOUBLE PRECISION",
                "sum" => "SUM(value)::DOUBLE PRECISION",
                "last" => "(ARRAY_AGG(value ORDER BY timestamp DESC))[1]::DOUBLE PRECISION",
                "count" => "COUNT(value)::DOUBLE PRECISION",
                "p50" => "(percentile_cont(0.5) WITHIN GROUP (ORDER BY value))::DOUBLE PRECISION",
                "p90" => "(percentile_cont(0.9) WITHIN GROUP (ORDER BY value))::DOUBLE PRECISION",
                "p99" => "(percentile_cont(0.99) WITHIN GROUP (ORDER BY value))::DOUBLE PRECISION",
                _ => unreachable!(),
            };

            const MAX_POINTS_PER_SERIES: i64 = 1000;
            const MAX_SERIES: i64 = 50;

            let mut builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(
                "WITH filtered AS (\n  SELECT\n    to_timestamp(floor(extract(epoch from timestamp) / ",
            );
            builder.push_bind(step_seconds);
            builder.push(") * ");
            builder.push_bind(step_seconds);
            builder.push(
                ") AS bucket_ts,\n    labels,\n    value,\n    timestamp\n  FROM metrics\n  WHERE project_id = ",
            );
            builder.push_bind(project_id);
            builder.push(" AND environment = 'default'");
            builder.push(" AND name = ");
            builder.push_bind(q.name);
            builder.push(" AND timestamp >= ");
            builder.push_bind(from_ts);
            builder.push(" AND timestamp <= ");
            builder.push_bind(to_ts);
            if let Some(f) = &labels_filter {
                builder.push(" AND labels @> ");
                builder.push_bind(f.clone());
            }

            builder.push("),\naggregated AS (\n  SELECT\n    bucket_ts,\n    ");
            if group_by_keys.is_empty() {
                builder.push("labels,\n    ");
            } else {
                builder.push("jsonb_build_object(");
                for (idx, group_key) in group_by_keys.iter().enumerate() {
                    if idx > 0 {
                        builder.push(", ");
                    }
                    builder.push(sql_string_literal(group_key));
                    builder.push(", labels ->> ");
                    builder.push(sql_string_literal(group_key));
                }
                builder.push(") AS labels,\n    ");
            }
            builder.push(agg_expr);
            builder.push(" AS value\n  FROM filtered\n  GROUP BY bucket_ts");
            if group_by_keys.is_empty() {
                builder.push(", labels");
            } else {
                for group_key in &group_by_keys {
                    builder.push(", labels ->> ");
                    builder.push(sql_string_literal(group_key));
                }
            }
            builder.push(
                "\n),\nranked AS (\n  SELECT\n    bucket_ts,\n    labels,\n    value,\n    ROW_NUMBER() OVER (PARTITION BY labels ORDER BY bucket_ts ASC) AS point_rank,\n    DENSE_RANK() OVER (ORDER BY labels) AS series_rank\n  FROM aggregated\n)\nSELECT\n  bucket_ts,\n  labels,\n  value,\n  point_rank,\n  series_rank\nFROM ranked\nWHERE point_rank <= ",
            );
            builder.push_bind(MAX_POINTS_PER_SERIES);
            builder.push(" AND series_rank <= ");
            builder.push_bind(MAX_SERIES);
            builder.push("\nORDER BY labels, bucket_ts ASC");

            #[derive(Debug, sqlx::FromRow)]
            struct MetricsQueryLimitedRow {
                bucket_ts: DateTime<Utc>,
                labels: JsonValue,
                value: f64,
                point_rank: i64,
                series_rank: i64,
            }

            let rows: Vec<MetricsQueryLimitedRow> =
                builder.build_query_as().fetch_all(pool).await?;

            for r in rows {
                if r.point_rank > MAX_POINTS_PER_SERIES {
                    points_truncated = true;
                    continue;
                }
                if r.series_rank > MAX_SERIES {
                    series_truncated = true;
                    continue;
                }

                match latest_bucket {
                    Some(prev) if r.bucket_ts > prev => latest_bucket = Some(r.bucket_ts),
                    None => latest_bucket = Some(r.bucket_ts),
                    _ => {}
                }

                let key = group_key_from_json(&r.labels, &group_by_keys);
                let entry = series_map.entry(key).or_insert_with(|| MetricsSeries {
                    labels: r.labels.clone(),
                    values: Vec::new(),
                });

                entry.values.push(MetricValuePoint {
                    timestamp: r.bucket_ts.to_rfc3339(),
                    value: r.value,
                });
            }
        }
        crate::state::DatabaseConnection::Memory(mem_db) => {
            let metrics_guard = mem_db
                .metrics
                .lock()
                .map_err(|e| ApiError::Internal(format!("Mutex lock error: {e}")))?;

            let mut grouped: BTreeMap<String, MetricGroupBucket> = BTreeMap::new();

            for m in metrics_guard.iter() {
                if m.project_id != project_id
                    || m.environment != "default"
                    || m.name != q.name
                    || m.timestamp < from_ts
                    || m.timestamp > to_ts
                {
                    continue;
                }

                if let Some(ref filter) = labels_filter {
                    if !json_contains(&m.labels, filter) {
                        continue;
                    }
                }

                let bucket_seconds = (m.timestamp.timestamp() / step_seconds) * step_seconds;
                let bucket_ts =
                    DateTime::<Utc>::from_timestamp(bucket_seconds, 0).unwrap_or(m.timestamp);

                let (final_labels, key) = labels_and_group_key_from_json(&m.labels, &group_by_keys);
                let entry = grouped
                    .entry(key)
                    .or_insert_with(|| (final_labels, BTreeMap::new()));
                entry
                    .1
                    .entry(bucket_ts)
                    .or_default()
                    .push((m.value, m.timestamp));
            }

            let total_series = grouped.len();
            if total_series > 50 {
                series_truncated = true;
            }

            for (key, (final_labels, buckets)) in grouped.into_iter().take(50) {
                let mut values_vec = Vec::new();
                let total_buckets = buckets.len();
                if total_buckets > 1000 {
                    points_truncated = true;
                }

                for (bucket_ts, mut pts) in buckets.into_iter().take(1000) {
                    match latest_bucket {
                        Some(prev) if bucket_ts > prev => latest_bucket = Some(bucket_ts),
                        None => latest_bucket = Some(bucket_ts),
                        _ => {}
                    }

                    let val = match agg {
                        "avg" => {
                            let sum: f64 = pts.iter().map(|p| p.0).sum();
                            sum / pts.len() as f64
                        }
                        "max" => pts.iter().map(|p| p.0).fold(f64::MIN, |a, b| a.max(b)),
                        "min" => pts.iter().map(|p| p.0).fold(f64::MAX, |a, b| a.min(b)),
                        "sum" => pts.iter().map(|p| p.0).sum(),
                        "last" => {
                            pts.sort_by_key(|p| p.1);
                            pts.last().map(|p| p.0).unwrap_or(0.0)
                        }
                        "count" => pts.len() as f64,
                        "p50" => {
                            let vals: Vec<f64> = pts.iter().map(|p| p.0).collect();
                            percentile(&vals, 0.5)
                        }
                        "p90" => {
                            let vals: Vec<f64> = pts.iter().map(|p| p.0).collect();
                            percentile(&vals, 0.9)
                        }
                        "p99" => {
                            let vals: Vec<f64> = pts.iter().map(|p| p.0).collect();
                            percentile(&vals, 0.99)
                        }
                        _ => 0.0,
                    };

                    values_vec.push(MetricValuePoint {
                        timestamp: bucket_ts.to_rfc3339(),
                        value: val,
                    });
                }

                series_map.insert(
                    key,
                    MetricsSeries {
                        labels: final_labels,
                        values: values_vec,
                    },
                );
            }
        }
    }

    let data = series_map.into_values().collect::<Vec<_>>();

    let meta = MetricsQueryMeta {
        latest_ts: latest_bucket.map(|ts| ts.to_rfc3339()),
        series_count: data.len(),
        truncated: points_truncated || series_truncated,
    };

    Ok((StatusCode::OK, Json(MetricsQueryResponse { data, meta })))
}

#[derive(Debug, Deserialize)]
#[allow(non_snake_case)]
pub(crate) struct MetricsDailyQuery {
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,

    #[serde(default, rename = "traceName")]
    trace_name: Option<String>,
    #[serde(default, rename = "userId")]
    user_id: Option<String>,
    #[serde(default)]
    tags: Vec<String>,

    #[serde(default, rename = "fromTimestamp")]
    from_timestamp: Option<DateTime<Utc>>,
    #[serde(default, rename = "toTimestamp")]
    to_timestamp: Option<DateTime<Utc>>,

    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    release: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct MetricsDailyRow {
    day: NaiveDate,
    count_traces: i64,
    count_observations: i64,
    total_cost: f64,
    usage: JsonValue,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricsDailyItem {
    date: String,
    count_traces: i64,
    count_observations: i64,
    total_cost: f64,
    usage: JsonValue,
}

pub(crate) async fn get_metrics_daily(
    State(state): State<AppState>,
    Query(q): Query<MetricsDailyQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let page = q.page.unwrap_or(1).max(1);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * limit;

    let now = Utc::now();
    let to_ts = q.to_timestamp.unwrap_or(now);
    let from_ts = q
        .from_timestamp
        .unwrap_or_else(|| to_ts - chrono::Duration::days(30));

    let project_id = state.default_project_id.as_ref();

    let (total_items, total_pages, items) = match &state.db {
        crate::state::DatabaseConnection::Postgres(pool) => {
            let mut count_builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(
                "SELECT COUNT(*)::BIGINT FROM (SELECT date_trunc('day', t.\"timestamp\")::date AS day FROM traces t WHERE 1=1",
            );
            count_builder.push(" AND t.project_id = ");
            count_builder.push_bind(project_id.to_string());
            count_builder.push(" AND t.\"timestamp\" >= ");
            count_builder.push_bind(from_ts);
            count_builder.push(" AND t.\"timestamp\" <= ");
            count_builder.push_bind(to_ts);

            if let Some(trace_name) = &q.trace_name {
                count_builder.push(" AND t.name = ");
                count_builder.push_bind(trace_name.clone());
            }
            if let Some(user_id) = &q.user_id {
                count_builder.push(" AND t.user_id = ");
                count_builder.push_bind(user_id.clone());
            }
            if !q.tags.is_empty() {
                count_builder.push(" AND t.tags @> ");
                count_builder.push_bind(q.tags.clone());
            }
            if let Some(version) = &q.version {
                count_builder.push(" AND t.version = ");
                count_builder.push_bind(version.clone());
            }
            if let Some(release) = &q.release {
                count_builder.push(" AND t.release = ");
                count_builder.push_bind(release.clone());
            }
            count_builder.push(" GROUP BY 1) x");

            let total_items: i64 = count_builder.build_query_scalar().fetch_one(pool).await?;

            let total_pages = if total_items == 0 {
                0
            } else {
                (total_items + limit - 1) / limit
            };

            let mut builder: QueryBuilder<'_, sqlx::Postgres> =
                QueryBuilder::new("WITH filtered_traces AS (SELECT t.* FROM traces t WHERE 1=1");
            builder.push(" AND t.project_id = ");
            builder.push_bind(project_id.to_string());
            builder.push(" AND t.\"timestamp\" >= ");
            builder.push_bind(from_ts);
            builder.push(" AND t.\"timestamp\" <= ");
            builder.push_bind(to_ts);

            if let Some(trace_name) = &q.trace_name {
                builder.push(" AND t.name = ");
                builder.push_bind(trace_name.clone());
            }
            if let Some(user_id) = &q.user_id {
                builder.push(" AND t.user_id = ");
                builder.push_bind(user_id.clone());
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

            builder.push(
                ")\n, daily AS (\n  SELECT\n    date_trunc('day', ft.\"timestamp\")::date AS day,\n    COUNT(*)::BIGINT AS count_traces,\n    COALESCE(SUM(ft.total_cost), 0)::DOUBLE PRECISION AS total_cost\n  FROM filtered_traces ft\n  GROUP BY 1\n)\n, daily_obs AS (\n  SELECT\n    date_trunc('day', ft.\"timestamp\")::date AS day,\n    COUNT(o.id)::BIGINT AS count_observations\n  FROM filtered_traces ft\n  JOIN observations o ON o.trace_id = ft.id\n  GROUP BY 1\n)\n, model_usage AS (\n  SELECT\n    date_trunc('day', ft.\"timestamp\")::date AS day,\n    COALESCE(o.model, 'unknown') AS model,\n    COALESCE(SUM(o.prompt_tokens), 0)::BIGINT AS input_usage,\n    COALESCE(SUM(o.completion_tokens), 0)::BIGINT AS output_usage,\n    COALESCE(SUM(o.total_tokens), 0)::BIGINT AS total_usage,\n    COUNT(DISTINCT ft.id)::BIGINT AS count_traces,\n    COUNT(o.id)::BIGINT AS count_observations,\n    COALESCE(SUM(o.calculated_total_cost), 0)::DOUBLE PRECISION AS total_cost\n  FROM filtered_traces ft\n  JOIN observations o ON o.trace_id = ft.id\n  WHERE o.type = 'GENERATION'\n  GROUP BY 1, 2\n)\n, daily_usage AS (\n  SELECT\n    mu.day,\n    COALESCE(jsonb_agg(\n      jsonb_build_object(\n        'model', mu.model,\n        'inputUsage', mu.input_usage,\n        'outputUsage', mu.output_usage,\n        'totalUsage', mu.total_usage,\n        'countTraces', mu.count_traces,\n        'countObservations', mu.count_observations,\n        'totalCost', mu.total_cost\n      ) ORDER BY mu.total_cost DESC\n    ), '[]'::jsonb) AS usage\n  FROM model_usage mu\n  GROUP BY 1\n)\nSELECT\n  d.day AS day,\n  d.count_traces AS count_traces,\n  COALESCE(dob.count_observations, 0) AS count_observations,\n  d.total_cost AS total_cost,\n  COALESCE(du.usage, '[]'::jsonb) AS usage\nFROM daily d\nLEFT JOIN daily_obs dob ON dob.day = d.day\nLEFT JOIN daily_usage du ON du.day = d.day\nORDER BY d.day DESC\nLIMIT ",
            );
            builder.push_bind(limit);
            builder.push(" OFFSET ");
            builder.push_bind(offset);

            let rows: Vec<MetricsDailyRow> = builder.build_query_as().fetch_all(pool).await?;

            let items = rows
                .into_iter()
                .map(|r| MetricsDailyItem {
                    date: r.day.to_string(),
                    count_traces: r.count_traces,
                    count_observations: r.count_observations,
                    total_cost: r.total_cost,
                    usage: r.usage,
                })
                .collect::<Vec<_>>();
            (total_items, total_pages, items)
        }
        crate::state::DatabaseConnection::Memory(mem_db) => {
            let filtered_traces: Vec<crate::state::TraceRow> = mem_db
                .traces
                .iter()
                .map(|item| item.value().clone())
                .filter(|t| {
                    if t.project_id != project_id || t.timestamp < from_ts || t.timestamp > to_ts {
                        return false;
                    }
                    if let Some(ref trace_name) = q.trace_name {
                        if t.name.as_ref() != Some(trace_name) {
                            return false;
                        }
                    }
                    if let Some(ref user_id) = q.user_id {
                        if t.user_id.as_ref() != Some(user_id) {
                            return false;
                        }
                    }
                    if !q.tags.is_empty() && !q.tags.iter().all(|tag| t.tags.contains(tag)) {
                        return false;
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
                    true
                })
                .collect();

            let mut trace_to_obs: HashMap<uuid::Uuid, Vec<crate::state::ObservationRow>> =
                HashMap::new();
            for entry in mem_db.observations.iter() {
                let obs = entry.value();
                trace_to_obs
                    .entry(obs.trace_id)
                    .or_default()
                    .push(obs.clone());
            }

            let mut daily_groups: BTreeMap<NaiveDate, DailyTraceGroup> = BTreeMap::new();
            for t in filtered_traces {
                let day = t.timestamp.date_naive();
                let entry = daily_groups.entry(day).or_default();
                if let Some(obs_list) = trace_to_obs.get(&t.id) {
                    entry.1.extend(obs_list.clone());
                }
                entry.0.push(t);
            }

            let mut daily_results = Vec::new();
            for (day, (traces, obs)) in daily_groups {
                let count_traces = traces.len() as i64;
                let count_observations = obs.len() as i64;
                let total_cost: f64 = traces.iter().map(|t| t.total_cost.unwrap_or(0.0)).sum();

                let mut model_groups: HashMap<String, DailyModelGroup> = HashMap::new();
                for o in obs.iter().filter(|o| o.r#type == "GENERATION") {
                    let model = o
                        .model
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "unknown".to_string());
                    let entry = model_groups.entry(model).or_insert((
                        0,
                        0,
                        0,
                        std::collections::HashSet::new(),
                        0,
                        0.0,
                    ));
                    entry.0 += o.prompt_tokens.unwrap_or(0);
                    entry.1 += o.completion_tokens.unwrap_or(0);
                    entry.2 += o.total_tokens.unwrap_or(0);
                    entry.3.insert(o.trace_id);
                    entry.4 += 1;
                    entry.5 += o.calculated_total_cost.unwrap_or(0.0);
                }

                let mut usage_list = Vec::new();
                for (
                    model,
                    (input_usage, output_usage, total_usage, trace_ids, count_obs, model_cost),
                ) in model_groups
                {
                    usage_list.push(serde_json::json!({
                        "model": model,
                        "inputUsage": input_usage,
                        "outputUsage": output_usage,
                        "totalUsage": total_usage,
                        "countTraces": trace_ids.len() as i64,
                        "countObservations": count_obs,
                        "totalCost": model_cost,
                    }));
                }

                usage_list.sort_by(|a, b| {
                    let a_cost = a["totalCost"].as_f64().unwrap_or(0.0);
                    let b_cost = b["totalCost"].as_f64().unwrap_or(0.0);
                    b_cost
                        .partial_cmp(&a_cost)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                daily_results.push(MetricsDailyItem {
                    date: day.to_string(),
                    count_traces,
                    count_observations,
                    total_cost,
                    usage: JsonValue::Array(usage_list),
                });
            }

            daily_results.sort_by(|a, b| b.date.cmp(&a.date));
            let total_items = daily_results.len() as i64;
            let total_pages = if total_items == 0 {
                0
            } else {
                (total_items + limit - 1) / limit
            };
            let paged = daily_results
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect::<Vec<_>>();
            (total_items, total_pages, paged)
        }
    };

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

fn labels_to_json(labels: HashMap<String, String>) -> JsonValue {
    let mut m = serde_json::Map::with_capacity(labels.len());
    for (k, v) in labels {
        m.insert(k, JsonValue::String(v));
    }
    JsonValue::Object(m)
}

pub(crate) async fn metrics_worker(
    db: crate::state::DatabaseConnection,
    default_project_id: Arc<str>,
    mut rx: mpsc::Receiver<MetricsBatchRequest>,
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

        if let Err(err) = write_metrics_batches(&db, default_project_id.as_ref(), batches).await {
            tracing::error!(error = ?err, "failed to write metrics batch");
        }
    }
}

async fn write_metrics_batches(
    db: &crate::state::DatabaseConnection,
    default_project_id: &str,
    payloads: Vec<MetricsBatchRequest>,
) -> Result<(), anyhow::Error> {
    let mut points: Vec<MetricPointIngest> = Vec::new();
    for p in payloads {
        points.extend(p.metrics);
    }
    if points.is_empty() {
        return Ok(());
    }

    match db {
        crate::state::DatabaseConnection::Postgres(pool) => {
            let mut tx = pool.begin().await?;

            let mut builder: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(
                "INSERT INTO metrics (project_id, environment, name, labels, value, timestamp) ",
            );
            builder.push_values(points, |mut b, m| {
                b.push_bind(default_project_id.to_string())
                    .push_bind("default".to_string())
                    .push_bind(m.name)
                    .push_bind(labels_to_json(m.labels))
                    .push_bind(m.value)
                    .push_bind(m.timestamp);
            });

            builder.build().execute(&mut *tx).await?;
            tx.commit().await?;
        }
        crate::state::DatabaseConnection::Memory(mem_db) => {
            let mut metrics_guard = mem_db
                .metrics
                .lock()
                .map_err(|e| anyhow::anyhow!("Mutex lock error: {e}"))?;
            let now = Utc::now();
            for m in points {
                metrics_guard.push(crate::state::MetricRow {
                    project_id: default_project_id.to_string(),
                    environment: "default".to_string(),
                    name: m.name,
                    labels: labels_to_json(m.labels),
                    value: m.value,
                    timestamp: m.timestamp,
                    created_at: now,
                });
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub(crate) struct MetricsOverviewQuery {
    pub(crate) query: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct MetricsOverviewQueryConfig {
    view: String,
    from_timestamp: Option<DateTime<Utc>>,
    to_timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    metrics: Vec<MetricSpec>,
    #[serde(default)]
    dimensions: Vec<DimensionSpec>,
    #[serde(default)]
    filters: Vec<FilterSpec>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct MetricSpec {
    measure: String,
    #[serde(default)]
    aggregation: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DimensionSpec {
    field: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FilterSpec {
    #[serde(rename = "type", default)]
    filter_type: String,
    #[serde(default)]
    column: String,
    #[serde(default)]
    operator: String,
    #[serde(default)]
    value: String,
}

pub(crate) async fn get_metrics_overview(
    State(state): State<AppState>,
    Query(q): Query<MetricsOverviewQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let config: MetricsOverviewQueryConfig =
        serde_json::from_str(&q.query).map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let project_id = state.default_project_id.to_string();
    let from_ts = config
        .from_timestamp
        .unwrap_or_else(|| Utc::now() - chrono::Duration::days(365));
    let to_ts = config.to_timestamp.unwrap_or_else(Utc::now);

    if config.view == "traces" {
        let res = match &state.db {
            crate::state::DatabaseConnection::Postgres(pool) => {
                let row: (i64, f64, f64, f64, i64) = sqlx::query_as(
                    r#"
SELECT
  COUNT(*)::BIGINT,
  COALESCE(AVG(latency), 0.0) * 1000.0,
  COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY latency), 0.0) * 1000.0,
  COALESCE(percentile_cont(0.99) WITHIN GROUP (ORDER BY latency), 0.0) * 1000.0,
  (
    SELECT COUNT(DISTINCT o.trace_id)::BIGINT
    FROM observations o
    WHERE o.project_id = $1
      AND o.level = 'ERROR'
      AND COALESCE(o.start_time, o.created_at) >= $2
      AND COALESCE(o.start_time, o.created_at) <= $3
  )
FROM traces
WHERE project_id = $1 AND timestamp >= $2 AND timestamp <= $3
                    "#,
                )
                .bind(&project_id)
                .bind(from_ts)
                .bind(to_ts)
                .fetch_one(pool)
                .await?;

                serde_json::json!({
                    "data": [
                        {
                            "count_count": row.0,
                            "avg_latency": row.1,
                            "p95_latency": row.2,
                            "p99_latency": row.3,
                            "error_count": row.4,
                        }
                    ]
                })
            }
            crate::state::DatabaseConnection::Memory(mem_db) => {
                let mut count = 0;
                let mut latencies = Vec::new();
                for entry in mem_db.traces.iter() {
                    let t = entry.value();
                    if t.project_id == project_id && t.timestamp >= from_ts && t.timestamp <= to_ts
                    {
                        count += 1;
                        if let Some(lat) = t.latency {
                            if lat.is_finite() && lat >= 0.0 {
                                latencies.push(lat);
                            }
                        }
                    }
                }

                let mut error_trace_ids = std::collections::HashSet::new();
                for entry in mem_db.observations.iter() {
                    let o = entry.value();
                    let obs_time = o.start_time.unwrap_or(o.created_at);
                    if o.project_id == project_id
                        && o.level.as_deref() == Some("ERROR")
                        && obs_time >= from_ts
                        && obs_time <= to_ts
                    {
                        error_trace_ids.insert(o.trace_id);
                    }
                }

                let (avg, p95, p99) = if latencies.is_empty() {
                    (0.0, 0.0, 0.0)
                } else {
                    let sum: f64 = latencies.iter().sum();
                    let avg = sum / latencies.len() as f64;
                    let p95 = percentile(&latencies, 0.95);
                    let p99 = percentile(&latencies, 0.99);
                    (avg, p95, p99)
                };

                serde_json::json!({
                    "data": [
                        {
                            "count_count": count,
                            "avg_latency": avg * 1000.0,
                            "p95_latency": p95 * 1000.0,
                            "p99_latency": p99 * 1000.0,
                            "error_count": error_trace_ids.len() as i64,
                        }
                    ]
                })
            }
        };

        Ok((StatusCode::OK, Json(res)))
    } else if config.view == "observations" {
        let is_error_filter = config
            .filters
            .iter()
            .any(|f| f.column == "level" && f.value == "ERROR");

        if is_error_filter {
            let trace_ids: Vec<String> = match &state.db {
                crate::state::DatabaseConnection::Postgres(pool) => {
                    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
                        r#"
SELECT DISTINCT trace_id
FROM observations
WHERE project_id = $1 AND level = 'ERROR' AND start_time >= $2 AND start_time <= $3
                        "#,
                    )
                    .bind(&project_id)
                    .bind(from_ts)
                    .bind(to_ts)
                    .fetch_all(pool)
                    .await?;

                    rows.into_iter().map(|r| r.0.to_string()).collect()
                }
                crate::state::DatabaseConnection::Memory(mem_db) => {
                    let mut ids = std::collections::HashSet::new();
                    for entry in mem_db.observations.iter() {
                        let o = entry.value();
                        let start_time = o.start_time.unwrap_or(o.created_at);
                        if o.project_id == project_id
                            && o.level.as_deref() == Some("ERROR")
                            && start_time >= from_ts
                            && start_time <= to_ts
                        {
                            ids.insert(o.trace_id.to_string());
                        }
                    }
                    ids.into_iter().collect()
                }
            };

            let data_list = trace_ids
                .into_iter()
                .map(|tid| {
                    serde_json::json!({
                        "traceId": tid
                    })
                })
                .collect::<Vec<_>>();

            let res = serde_json::json!({
                "data": data_list
            });
            Ok((StatusCode::OK, Json(res)))
        } else {
            let res = serde_json::json!({
                "data": []
            });
            Ok((StatusCode::OK, Json(res)))
        }
    } else {
        let res = serde_json::json!({
            "data": []
        });
        Ok((StatusCode::OK, Json(res)))
    }
}

fn json_contains(target: &JsonValue, filter: &JsonValue) -> bool {
    match (target, filter) {
        (JsonValue::Object(t_map), JsonValue::Object(f_map)) => {
            for (k, v) in f_map {
                match t_map.get(k) {
                    Some(t_val) => {
                        if t_val != v {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            true
        }
        _ => target == filter,
    }
}

fn percentile(vals: &[f64], p: f64) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut sorted = vals.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = (sorted.len() - 1) as f64 * p;
    let idx = pos.floor() as usize;
    let fract = pos - idx as f64;
    if idx + 1 < sorted.len() {
        sorted[idx] * (1.0 - fract) + sorted[idx + 1] * fract
    } else {
        sorted[idx]
    }
}
