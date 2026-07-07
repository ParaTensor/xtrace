use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use xtrace::metrics::label_governance::{
    parse_label_key_set, LabelGovernanceConfig, LabelOverflowPolicy,
};
use xtrace::{run_server, ServerConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info,sqlx=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let mock_storage = std::env::var("XTRACE_MOCK_STORAGE")
        .map(|s| s == "true")
        .unwrap_or(false);
    let json_dir_env = std::env::var("XTRACE_JSON_DIR").ok();
    let is_mock = mock_storage || json_dir_env.is_some();

    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            if is_mock {
                "postgres://dummy_mock_url".to_string()
            } else {
                return Err(anyhow::anyhow!("missing env DATABASE_URL"));
            }
        }
    };

    let api_bearer_token = match std::env::var("API_BEARER_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            if is_mock {
                let default_token = "xtrace-default-token".to_string();
                tracing::warn!(
                    "API_BEARER_TOKEN is not set. Defaulting to '{}' for mock storage mode.",
                    default_token
                );
                default_token
            } else {
                return Err(anyhow::anyhow!("missing env API_BEARER_TOKEN"));
            }
        }
    };

    let config = ServerConfig {
        database_url,
        api_bearer_token,
        bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8742".to_string()),
        default_project_id: std::env::var("DEFAULT_PROJECT_ID")
            .unwrap_or_else(|_| "default".to_string()),
        langfuse_public_key: std::env::var("XTRACE_PUBLIC_KEY")
            .ok()
            .or_else(|| std::env::var("LANGFUSE_PUBLIC_KEY").ok()),
        langfuse_secret_key: std::env::var("XTRACE_SECRET_KEY")
            .ok()
            .or_else(|| std::env::var("LANGFUSE_SECRET_KEY").ok()),
        rate_limit_qps: std::env::var("RATE_LIMIT_QPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20),
        rate_limit_burst: std::env::var("RATE_LIMIT_BURST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40),
        allow_unauthenticated_compat: std::env::var("XTRACE_ALLOW_UNAUTHENTICATED_COMPAT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
        max_request_body_bytes: std::env::var("XTRACE_MAX_REQUEST_BODY_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20 * 1024 * 1024),
        media_dir: std::env::var("XTRACE_MEDIA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                if is_mock {
                    let base = json_dir_env
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::path::PathBuf::from("./.xtrace_data"));
                    base.join("media")
                } else {
                    std::path::PathBuf::from(".xtrace_media")
                }
            }),
        public_base_url: std::env::var("XTRACE_PUBLIC_BASE_URL").ok(),
        media_max_content_length: std::env::var("XTRACE_MEDIA_MAX_CONTENT_LENGTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20 * 1024 * 1024),
        label_governance: LabelGovernanceConfig {
            max_labels_per_point: std::env::var("XTRACE_METRICS_MAX_LABELS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(64),
            max_label_value_len: std::env::var("XTRACE_METRICS_MAX_LABEL_VALUE_LEN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(256),
            allow_keys: parse_label_key_set(
                std::env::var("XTRACE_METRICS_LABEL_ALLOWLIST")
                    .ok()
                    .as_deref(),
            ),
            deny_keys: parse_label_key_set(
                std::env::var("XTRACE_METRICS_LABEL_DENYLIST")
                    .ok()
                    .as_deref(),
            )
            .unwrap_or_default(),
            overflow_policy: std::env::var("XTRACE_METRICS_LABEL_OVERFLOW_POLICY")
                .ok()
                .and_then(|v| LabelOverflowPolicy::parse(&v))
                .unwrap_or(LabelOverflowPolicy::DropLabels),
        },
        metrics_query_max_series: std::env::var("XTRACE_METRICS_QUERY_MAX_SERIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50),
        metrics_query_max_points_per_series: std::env::var("XTRACE_METRICS_QUERY_MAX_POINTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000),
        api_read_bearer_token: std::env::var("API_READ_BEARER_TOKEN").ok(),
        project_tokens: std::env::var("XTRACE_PROJECT_TOKENS").ok(),
        project_basic_auth: std::env::var("XTRACE_PROJECT_BASIC_AUTH").ok(),
        retention: xtrace::RetentionConfig {
            metrics_retention_days: std::env::var("METRICS_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            traces_retention_days: std::env::var("TRACES_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            metrics_downsample_after_days: std::env::var("METRICS_DOWNSAMPLE_AFTER_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            interval_hours: std::env::var("RETENTION_INTERVAL_HOURS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(24),
            memory_max_metrics: std::env::var("XTRACE_MEMORY_MAX_METRICS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            memory_max_traces: std::env::var("XTRACE_MEMORY_MAX_TRACES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        },
    };

    run_server(config).await
}
