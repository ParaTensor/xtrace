use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    sync::Arc,
};

use crate::http::{error::ApiError, metrics::MetricPointIngest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelOverflowPolicy {
    DropLabels,
    RejectPoint,
    RejectBatch,
}

impl LabelOverflowPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "drop_labels" | "drop" => Some(Self::DropLabels),
            "reject_point" | "reject-point" => Some(Self::RejectPoint),
            "reject_batch" | "reject-batch" => Some(Self::RejectBatch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LabelGovernanceConfig {
    pub max_labels_per_point: usize,
    pub max_label_value_len: usize,
    /// When set, only these label keys are accepted.
    pub allow_keys: Option<HashSet<String>>,
    pub deny_keys: HashSet<String>,
    pub overflow_policy: LabelOverflowPolicy,
}

impl Default for LabelGovernanceConfig {
    fn default() -> Self {
        Self {
            max_labels_per_point: 64,
            max_label_value_len: 256,
            allow_keys: None,
            deny_keys: HashSet::new(),
            overflow_policy: LabelOverflowPolicy::DropLabels,
        }
    }
}

#[derive(Default)]
pub struct LabelGovernanceStats {
    pub labels_dropped: AtomicU64,
    pub points_rejected: AtomicU64,
    pub batches_rejected: AtomicU64,
}

impl LabelGovernanceStats {
    pub fn snapshot(&self) -> LabelGovernanceStatsSnapshot {
        LabelGovernanceStatsSnapshot {
            labels_dropped: self.labels_dropped.load(Ordering::Relaxed),
            points_rejected: self.points_rejected.load(Ordering::Relaxed),
            batches_rejected: self.batches_rejected.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct LabelGovernanceStatsSnapshot {
    pub labels_dropped: u64,
    pub points_rejected: u64,
    pub batches_rejected: u64,
}

#[derive(Clone)]
pub struct LabelGovernance {
    pub config: LabelGovernanceConfig,
    pub stats: Arc<LabelGovernanceStats>,
}

impl Default for LabelGovernance {
    fn default() -> Self {
        Self::new(LabelGovernanceConfig::default())
    }
}

impl LabelGovernance {
    pub fn new(config: LabelGovernanceConfig) -> Self {
        Self {
            config,
            stats: Arc::new(LabelGovernanceStats::default()),
        }
    }

    pub(crate) fn apply_batch(
        &self,
        points: Vec<MetricPointIngest>,
    ) -> Result<Vec<MetricPointIngest>, ApiError> {
        let mut out = Vec::with_capacity(points.len());
        for point in points {
            if let Some(p) = self.apply_point(point)? {
                out.push(p);
            }
        }
        Ok(out)
    }

    fn apply_point(
        &self,
        mut point: MetricPointIngest,
    ) -> Result<Option<MetricPointIngest>, ApiError> {
        let (labels, dropped) = match self.govern_labels(point.labels) {
            Ok(v) => v,
            Err(e) if self.config.overflow_policy == LabelOverflowPolicy::RejectBatch => {
                self.stats.batches_rejected.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
            Err(_) => {
                self.stats.points_rejected.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    metric = %point.name,
                    "metrics point rejected by label governance"
                );
                return Ok(None);
            }
        };

        if !dropped.is_empty() {
            let n = dropped.len() as u64;
            self.stats.labels_dropped.fetch_add(n, Ordering::Relaxed);
            tracing::debug!(
                metric = %point.name,
                dropped_keys = ?dropped,
                count = n,
                "metrics labels dropped by label governance"
            );
        }

        point.labels = labels;
        Ok(Some(point))
    }

    fn govern_labels(
        &self,
        labels: HashMap<String, String>,
    ) -> Result<(HashMap<String, String>, Vec<String>), ApiError> {
        let cfg = &self.config;

        if cfg.overflow_policy == LabelOverflowPolicy::RejectBatch {
            self.check_violations(&labels)?;
        }

        let mut dropped = Vec::new();
        let mut kept: Vec<(String, String)> = Vec::with_capacity(labels.len());

        for (key, value) in labels {
            if cfg.deny_keys.contains(&key) {
                if cfg.overflow_policy == LabelOverflowPolicy::RejectPoint {
                    return Err(ApiError::BadRequest(format!(
                        "metric label key denied: {key}"
                    )));
                }
                dropped.push(key);
                continue;
            }
            if let Some(allow) = &cfg.allow_keys {
                if !allow.contains(&key) {
                    if cfg.overflow_policy == LabelOverflowPolicy::RejectPoint {
                        return Err(ApiError::BadRequest(format!(
                            "metric label key not allowed: {key}"
                        )));
                    }
                    dropped.push(key);
                    continue;
                }
            }
            if value.len() > cfg.max_label_value_len {
                if cfg.overflow_policy == LabelOverflowPolicy::RejectPoint {
                    return Err(ApiError::BadRequest(format!(
                        "metric label value too long for key: {key}"
                    )));
                }
                dropped.push(key);
                continue;
            }
            kept.push((key, value));
        }

        if kept.len() > cfg.max_labels_per_point {
            match cfg.overflow_policy {
                LabelOverflowPolicy::RejectPoint | LabelOverflowPolicy::RejectBatch => {
                    let msg = format!(
                        "metric has too many labels ({} > {})",
                        kept.len(),
                        cfg.max_labels_per_point
                    );
                    return Err(ApiError::BadRequest(msg));
                }
                LabelOverflowPolicy::DropLabels => {
                    kept.sort_by(|a, b| a.0.cmp(&b.0));
                    for (key, _) in kept.drain(cfg.max_labels_per_point..) {
                        dropped.push(key);
                    }
                }
            }
        }

        Ok((kept.into_iter().collect(), dropped))
    }

    fn check_violations(&self, labels: &HashMap<String, String>) -> Result<(), ApiError> {
        let cfg = &self.config;
        for (key, value) in labels {
            if cfg.deny_keys.contains(key) {
                return Err(ApiError::BadRequest(format!(
                    "metric label key denied: {key}"
                )));
            }
            if let Some(allow) = &cfg.allow_keys {
                if !allow.contains(key) {
                    return Err(ApiError::BadRequest(format!(
                        "metric label key not allowed: {key}"
                    )));
                }
            }
            if value.len() > cfg.max_label_value_len {
                return Err(ApiError::BadRequest(format!(
                    "metric label value too long for key: {key}"
                )));
            }
        }
        if labels.len() > cfg.max_labels_per_point {
            return Err(ApiError::BadRequest(format!(
                "metric has too many labels ({} > {})",
                labels.len(),
                cfg.max_labels_per_point
            )));
        }
        Ok(())
    }
}

pub fn parse_label_key_set(raw: Option<&str>) -> Option<HashSet<String>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let keys = raw
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .collect::<HashSet<_>>();
    if keys.is_empty() {
        None
    } else {
        Some(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn point(labels: HashMap<String, String>) -> MetricPointIngest {
        MetricPointIngest {
            name: "test".to_string(),
            labels,
            value: 1.0,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn drop_labels_removes_denylisted_keys() {
        let gov = LabelGovernance::new(LabelGovernanceConfig {
            deny_keys: HashSet::from(["user_id".to_string()]),
            ..Default::default()
        });
        let mut labels = HashMap::new();
        labels.insert("user_id".to_string(), "u1".to_string());
        labels.insert("node".to_string(), "n1".to_string());
        let out = gov.apply_point(point(labels)).unwrap().unwrap();
        assert_eq!(out.labels.len(), 1);
        assert_eq!(out.labels.get("node").map(String::as_str), Some("n1"));
        assert_eq!(gov.stats.labels_dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reject_point_fails_on_denylisted_key() {
        let gov = LabelGovernance::new(LabelGovernanceConfig {
            deny_keys: HashSet::from(["request_id".to_string()]),
            overflow_policy: LabelOverflowPolicy::RejectPoint,
            ..Default::default()
        });
        let mut labels = HashMap::new();
        labels.insert("request_id".to_string(), "r1".to_string());
        assert!(gov.apply_point(point(labels)).unwrap().is_none());
        assert_eq!(gov.stats.points_rejected.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drop_labels_truncates_excess_keys() {
        let gov = LabelGovernance::new(LabelGovernanceConfig {
            max_labels_per_point: 2,
            ..Default::default()
        });
        let labels = HashMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
            ("c".to_string(), "3".to_string()),
        ]);
        let out = gov.apply_point(point(labels)).unwrap().unwrap();
        assert_eq!(out.labels.len(), 2);
        assert_eq!(gov.stats.labels_dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reject_batch_fails_entire_batch() {
        let gov = LabelGovernance::new(LabelGovernanceConfig {
            deny_keys: HashSet::from(["user_id".to_string()]),
            overflow_policy: LabelOverflowPolicy::RejectBatch,
            ..Default::default()
        });
        let mut bad = HashMap::new();
        bad.insert("user_id".to_string(), "u1".to_string());
        let good = HashMap::from([("node".to_string(), "n1".to_string())]);
        let err = gov.apply_batch(vec![point(good), point(bad)]).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
        assert_eq!(gov.stats.batches_rejected.load(Ordering::Relaxed), 1);
    }
}
