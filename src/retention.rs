use chrono::{Duration, Utc};
use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::state::DatabaseConnection;

#[derive(Debug, Clone)]
pub struct RetentionConfig {
    pub metrics_retention_days: u32,
    pub traces_retention_days: u32,
    pub metrics_downsample_after_days: u32,
    pub interval_hours: u64,
    pub memory_max_metrics: usize,
    pub memory_max_traces: usize,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            metrics_retention_days: 0,
            traces_retention_days: 0,
            metrics_downsample_after_days: 0,
            interval_hours: 24,
            memory_max_metrics: 0,
            memory_max_traces: 0,
        }
    }
}

impl RetentionConfig {
    pub fn enabled(&self) -> bool {
        self.metrics_retention_days > 0
            || self.traces_retention_days > 0
            || self.metrics_downsample_after_days > 0
            || self.memory_max_metrics > 0
            || self.memory_max_traces > 0
    }
}

#[derive(Default)]
pub struct RetentionStats {
    pub runs: AtomicU64,
    pub metrics_deleted: AtomicU64,
    pub traces_deleted: AtomicU64,
    pub observations_deleted: AtomicU64,
    pub metrics_downsampled: AtomicU64,
    pub memory_metrics_evicted: AtomicU64,
    pub memory_traces_evicted: AtomicU64,
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl RetentionStats {
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "runs": self.runs.load(Ordering::Relaxed),
            "metrics_deleted": self.metrics_deleted.load(Ordering::Relaxed),
            "traces_deleted": self.traces_deleted.load(Ordering::Relaxed),
            "observations_deleted": self.observations_deleted.load(Ordering::Relaxed),
            "metrics_downsampled": self.metrics_downsampled.load(Ordering::Relaxed),
            "memory_metrics_evicted": self.memory_metrics_evicted.load(Ordering::Relaxed),
            "memory_traces_evicted": self.memory_traces_evicted.load(Ordering::Relaxed),
            "last_error": self.last_error.lock().ok().and_then(|g| g.clone()),
        })
    }
}

pub async fn retention_worker(
    db: DatabaseConnection,
    config: RetentionConfig,
    stats: Arc<RetentionStats>,
) {
    if !config.enabled() {
        return;
    }

    let interval = std::time::Duration::from_secs(config.interval_hours.max(1) * 3600);
    loop {
        tokio::time::sleep(interval).await;
        if let Err(err) = run_retention_pass(&db, &config, &stats).await {
            tracing::warn!(error = %err, "retention pass failed");
            if let Ok(mut guard) = stats.last_error.lock() {
                *guard = Some(err.to_string());
            }
        }
    }
}

async fn run_retention_pass(
    db: &DatabaseConnection,
    config: &RetentionConfig,
    stats: &RetentionStats,
) -> anyhow::Result<()> {
    stats.runs.fetch_add(1, Ordering::Relaxed);
    match db {
        DatabaseConnection::Postgres(pool) => {
            run_postgres_retention(pool, config, stats).await?;
        }
        DatabaseConnection::Memory(mem_db) => {
            run_memory_retention(mem_db, config, stats);
        }
    }
    Ok(())
}

async fn run_postgres_retention(
    pool: &PgPool,
    config: &RetentionConfig,
    stats: &RetentionStats,
) -> anyhow::Result<()> {
    let now = Utc::now();

    if config.metrics_downsample_after_days > 0
        && config.metrics_retention_days > config.metrics_downsample_after_days
    {
        let downsample_cutoff =
            now - Duration::days(i64::from(config.metrics_downsample_after_days));
        let retention_cutoff = now - Duration::days(i64::from(config.metrics_retention_days));
        let inserted = sqlx::query(
            r#"
INSERT INTO metrics (project_id, environment, name, labels, value, timestamp)
SELECT project_id, environment, name, labels, AVG(value), date_trunc('hour', timestamp)
FROM metrics
WHERE timestamp >= $1 AND timestamp < $2
GROUP BY project_id, environment, name, labels, date_trunc('hour', timestamp)
            "#,
        )
        .bind(retention_cutoff)
        .bind(downsample_cutoff)
        .execute(pool)
        .await?;
        stats
            .metrics_downsampled
            .fetch_add(inserted.rows_affected(), Ordering::Relaxed);

        let deleted = sqlx::query(
            r#"
DELETE FROM metrics
WHERE timestamp >= $1 AND timestamp < $2
            "#,
        )
        .bind(retention_cutoff)
        .bind(downsample_cutoff)
        .execute(pool)
        .await?;
        stats
            .metrics_deleted
            .fetch_add(deleted.rows_affected(), Ordering::Relaxed);
    }

    if config.metrics_retention_days > 0 {
        let cutoff = now - Duration::days(i64::from(config.metrics_retention_days));
        let deleted = sqlx::query("DELETE FROM metrics WHERE timestamp < $1")
            .bind(cutoff)
            .execute(pool)
            .await?;
        stats
            .metrics_deleted
            .fetch_add(deleted.rows_affected(), Ordering::Relaxed);
    }

    if config.traces_retention_days > 0 {
        let cutoff = now - Duration::days(i64::from(config.traces_retention_days));
        let obs_deleted = sqlx::query(
            "DELETE FROM observations WHERE trace_id IN (SELECT id FROM traces WHERE timestamp < $1)",
        )
        .bind(cutoff)
        .execute(pool)
        .await?;
        stats
            .observations_deleted
            .fetch_add(obs_deleted.rows_affected(), Ordering::Relaxed);

        let traces_deleted = sqlx::query("DELETE FROM traces WHERE timestamp < $1")
            .bind(cutoff)
            .execute(pool)
            .await?;
        stats
            .traces_deleted
            .fetch_add(traces_deleted.rows_affected(), Ordering::Relaxed);
    }

    Ok(())
}

fn run_memory_retention(
    mem_db: &Arc<crate::state::MemoryDb>,
    config: &RetentionConfig,
    stats: &RetentionStats,
) {
    let now = Utc::now();

    if config.metrics_retention_days > 0 {
        let cutoff = now - Duration::days(i64::from(config.metrics_retention_days));
        if let Ok(mut metrics) = mem_db.metrics.lock() {
            let before = metrics.len();
            metrics.retain(|m| m.timestamp >= cutoff);
            stats
                .metrics_deleted
                .fetch_add((before - metrics.len()) as u64, Ordering::Relaxed);
        }
    }

    if config.traces_retention_days > 0 {
        let cutoff = now - Duration::days(i64::from(config.traces_retention_days));
        let trace_ids: Vec<uuid::Uuid> = mem_db
            .traces
            .iter()
            .filter(|entry| entry.value().timestamp < cutoff)
            .map(|entry| *entry.key())
            .collect();
        for trace_id in &trace_ids {
            mem_db.traces.remove(trace_id);
        }
        stats
            .traces_deleted
            .fetch_add(trace_ids.len() as u64, Ordering::Relaxed);

        let obs_ids: Vec<uuid::Uuid> = mem_db
            .observations
            .iter()
            .filter(|entry| {
                trace_ids.contains(&entry.value().trace_id)
                    || entry.value().start_time.is_some_and(|t| t < cutoff)
            })
            .map(|entry| *entry.key())
            .collect();
        for obs_id in &obs_ids {
            mem_db.observations.remove(obs_id);
        }
        stats
            .observations_deleted
            .fetch_add(obs_ids.len() as u64, Ordering::Relaxed);
    }

    if config.memory_max_metrics > 0 {
        if let Ok(mut metrics) = mem_db.metrics.lock() {
            if metrics.len() > config.memory_max_metrics {
                metrics.sort_by_key(|m| m.timestamp);
                let evict = metrics.len() - config.memory_max_metrics;
                metrics.drain(0..evict);
                stats
                    .memory_metrics_evicted
                    .fetch_add(evict as u64, Ordering::Relaxed);
            }
        }
    }

    if config.memory_max_traces > 0 && mem_db.traces.len() > config.memory_max_traces {
        let mut entries: Vec<(uuid::Uuid, chrono::DateTime<Utc>)> = mem_db
            .traces
            .iter()
            .map(|entry| (*entry.key(), entry.value().timestamp))
            .collect();
        entries.sort_by_key(|(_, ts)| *ts);
        let evict = entries.len() - config.memory_max_traces;
        for (trace_id, _) in entries.into_iter().take(evict) {
            mem_db.traces.remove(&trace_id);
            mem_db
                .observations
                .retain(|_, obs| obs.trace_id != trace_id);
        }
        stats
            .memory_traces_evicted
            .fetch_add(evict as u64, Ordering::Relaxed);
    }
}
