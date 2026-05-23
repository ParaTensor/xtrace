use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
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
    };

    run_server(config).await
}
