use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use std::sync::atomic::Ordering;

use crate::state::AppState;

pub(crate) async fn get_ingest_stats(State(state): State<AppState>) -> impl IntoResponse {
    let enqueued = state.ingest_stats.enqueued.load(Ordering::Relaxed);
    let queue_rejected = state.ingest_stats.queue_rejected.load(Ordering::Relaxed);
    let batches_written = state.ingest_stats.batches_written.load(Ordering::Relaxed);
    let batches_failed = state.ingest_stats.batches_failed.load(Ordering::Relaxed);
    let items_written = state.ingest_stats.items_written.load(Ordering::Relaxed);

    let body = serde_json::json!({
        "enqueued": enqueued,
        "queue_rejected": queue_rejected,
        "batches_written": batches_written,
        "batches_failed": batches_failed,
        "items_written": items_written,
        "failure_rate": if batches_written + batches_failed > 0 {
            batches_failed as f64 / (batches_written + batches_failed) as f64
        } else {
            0.0
        },
    });

    (StatusCode::OK, Json(body))
}

pub(crate) async fn get_rate_limit_stats(State(state): State<AppState>) -> impl IntoResponse {
    let total_allowed = state.rate_limit_stats.total_allowed.load(Ordering::Relaxed);
    let total_rejected = state
        .rate_limit_stats
        .total_rejected
        .load(Ordering::Relaxed);

    let mut per_token: Vec<(String, u64)> = state
        .rate_limit_stats
        .per_token_rejected
        .iter()
        .map(|entry| (entry.key().clone(), *entry.value()))
        .collect();
    per_token.sort_by_key(|b| std::cmp::Reverse(b.1));

    let top = per_token
        .into_iter()
        .take(20)
        .map(|(token, count)| serde_json::json!({ "token": token, "count": count }))
        .collect::<Vec<_>>();

    let body = serde_json::json!({
        "rate_limit_qps": state.rate_limit_qps,
        "rate_limit_burst": state.rate_limit_burst,
        "total_allowed": total_allowed,
        "total_rejected": total_rejected,
        "rejection_rate": if total_allowed + total_rejected > 0 {
            total_rejected as f64 / (total_allowed + total_rejected) as f64
        } else {
            0.0
        },
        "top_rejected_tokens": top,
    });

    (StatusCode::OK, Json(body))
}

pub(crate) async fn get_metrics_label_governance_stats(
    State(state): State<AppState>,
) -> impl IntoResponse {
    use crate::metrics::label_governance::LabelOverflowPolicy;

    let stats = state.label_governance.stats.snapshot();
    let cfg = &state.label_governance.config;

    let body = serde_json::json!({
        "labels_dropped": stats.labels_dropped,
        "points_rejected": stats.points_rejected,
        "batches_rejected": stats.batches_rejected,
        "max_labels_per_point": cfg.max_labels_per_point,
        "max_label_value_len": cfg.max_label_value_len,
        "overflow_policy": match cfg.overflow_policy {
            LabelOverflowPolicy::DropLabels => "drop_labels",
            LabelOverflowPolicy::RejectPoint => "reject_point",
            LabelOverflowPolicy::RejectBatch => "reject_batch",
        },
        "allow_keys_configured": cfg.allow_keys.is_some(),
        "deny_keys_count": cfg.deny_keys.len(),
        "metrics_query_max_series": state.metrics_query_max_series,
        "metrics_query_max_points_per_series": state.metrics_query_max_points_per_series,
    });

    (StatusCode::OK, Json(body))
}

pub(crate) async fn get_retention_stats(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.retention_stats.snapshot()))
}
