use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{PgPool, QueryBuilder};
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
        "p50" => Ok("p50"),
        "p90" => Ok("p90"),
        "p99" => Ok("p99"),
        _ => Err(ApiError::BadRequest(
            "invalid agg, must be one of: avg, max, min, sum, last, p50, p90, p99".to_string(),
        )),
    }
}

pub(crate) async fn get_metrics_names(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = state.default_project_id.as_ref();

    let names: Vec<String> = sqlx::query_scalar(
        r#"
SELECT DISTINCT name
FROM metrics
WHERE project_id = $1 AND environment = 'default'
ORDER BY name
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.pool)
    .await?;

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

    let project_id = state.default_project_id.as_ref();

    let agg_expr = match agg {
        "avg" => "AVG(value)::DOUBLE PRECISION",
        "max" => "MAX(value)::DOUBLE PRECISION",
        "min" => "MIN(value)::DOUBLE PRECISION",
        "sum" => "SUM(value)::DOUBLE PRECISION",
        "last" => "(ARRAY_AGG(value ORDER BY timestamp DESC))[1]::DOUBLE PRECISION",
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
    if let Some(ref group_key) = q.group_by {
        builder.push("jsonb_build_object(");
        builder.push_bind(group_key.clone());
        builder.push(", labels ->> ");
        builder.push_bind(group_key.clone());
        builder.push(") AS labels,\n    ");
    } else {
        builder.push("labels,\n    ");
    }
    builder.push(agg_expr);
    builder.push(" AS value\n  FROM filtered\n  GROUP BY bucket_ts, ");
    if let Some(ref group_key) = q.group_by {
        builder.push("labels ->> ");
        builder.push_bind(group_key.clone());
    } else {
        builder.push("labels");
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

    let rows: Vec<MetricsQueryLimitedRow> = builder.build_query_as().fetch_all(&state.pool).await?;

    let mut series_map: BTreeMap<String, MetricsSeries> = BTreeMap::new();
    let mut points_truncated = false;
    let mut series_truncated = false;
    let mut latest_bucket: Option<DateTime<Utc>> = None;

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

        let key = r.labels.to_string();
        let entry = series_map.entry(key).or_insert_with(|| MetricsSeries {
            labels: r.labels.clone(),
            values: Vec::new(),
        });

        entry.values.push(MetricValuePoint {
            timestamp: r.bucket_ts.to_rfc3339(),
            value: r.value,
        });
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

    let total_items: i64 = count_builder
        .build_query_scalar()
        .fetch_one(&state.pool)
        .await?;

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

    let rows: Vec<MetricsDailyRow> = builder.build_query_as().fetch_all(&state.pool).await?;

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
    pool: PgPool,
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

        if let Err(err) = write_metrics_batches(&pool, default_project_id.as_ref(), batches).await {
            tracing::error!(error = ?err, "failed to write metrics batch");
        }
    }
}

async fn write_metrics_batches(
    pool: &PgPool,
    default_project_id: &str,
    payloads: Vec<MetricsBatchRequest>,
) -> Result<(), sqlx::Error> {
    let mut points: Vec<MetricPointIngest> = Vec::new();
    for p in payloads {
        points.extend(p.metrics);
    }
    if points.is_empty() {
        return Ok(());
    }

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
    Ok(())
}
