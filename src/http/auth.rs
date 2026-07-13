use axum::{
    extract::State,
    http::{header, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::IntoResponse,
    Json,
};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::Utc;

use crate::{
    http::{
        auth_context::{is_write_route, AuthAccess, AuthContext, AuthRegistry},
        common::ApiResponse,
    },
    state::{mask_client_key, AppState},
};

enum AuthHeader {
    Bearer(String),
    Basic { username: String, password: String },
}

fn extract_auth(headers: &HeaderMap) -> Result<AuthHeader, ()> {
    let value = headers
        .get(header::AUTHORIZATION)
        .ok_or(())
        .and_then(|v| v.to_str().map_err(|_| ()))?
        .trim();

    if let Some(rest) = value.strip_prefix("Bearer ") {
        return Ok(AuthHeader::Bearer(rest.trim().to_string()));
    }

    if let Some(rest) = value.strip_prefix("Basic ") {
        let decoded = BASE64_STANDARD
            .decode(rest.trim().as_bytes())
            .map_err(|_| ())?;
        let decoded = std::str::from_utf8(&decoded).map_err(|_| ())?;
        let (username, password) = decoded.split_once(':').ok_or(())?;
        return Ok(AuthHeader::Basic {
            username: username.to_string(),
            password: password.to_string(),
        });
    }

    Err(())
}

fn extract_client_key(headers: &HeaderMap) -> String {
    if let Ok(auth) = extract_auth(headers) {
        match auth {
            AuthHeader::Bearer(token) => return format!("bearer:{token}"),
            AuthHeader::Basic { username, .. } => return format!("basic:{username}"),
        }
    }
    "anonymous".to_string()
}

fn resolve_auth_context(
    registry: &AuthRegistry,
    headers: &HeaderMap,
) -> Option<AuthContext> {
    match extract_auth(headers) {
        Ok(AuthHeader::Bearer(token)) => registry.resolve_bearer(&token),
        Ok(AuthHeader::Basic { username, password }) => {
            registry.resolve_basic(&username, &password)
        }
        Err(()) => None,
    }
}

pub(crate) async fn auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    // CORS preflight has no Authorization; must not 401.
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }

    let path = request.uri().path();
    let method = request.method().clone();
    let is_langfuse_compat = matches!(
        path,
        "/api/public/projects" | "/api/public/otel/v1/traces" | "/api/public/otel/v1/metrics"
    );
    let langfuse_auth_not_configured = !state.auth_registry.has_basic_auth_configured()
        && state.langfuse_public_key.is_none()
        && state.langfuse_secret_key.is_none();
    let open_compat = state.allow_unauthenticated_compat && langfuse_auth_not_configured;

    if let Some(ctx) = resolve_auth_context(&state.auth_registry, &headers) {
        if is_write_route(&method, path) && ctx.access == AuthAccess::Read {
            return (
                StatusCode::FORBIDDEN,
                Json(ApiResponse::<serde_json::Value> {
                    message: "read-only token cannot write".to_string(),
                    code: Some("FORBIDDEN"),
                    data: None,
                }),
            )
                .into_response();
        }
        request.extensions_mut().insert(ctx);
        return next.run(request).await;
    }

    match extract_auth(&headers) {
        Err(()) if is_langfuse_compat && open_compat => {
            request.extensions_mut().insert(AuthContext {
                project_id: state.auth_registry.default_project_id.clone(),
                access: AuthAccess::Write,
            });
            next.run(request).await
        }
        Ok(AuthHeader::Basic { .. }) if is_langfuse_compat && open_compat => {
            request.extensions_mut().insert(AuthContext {
                project_id: state.auth_registry.default_project_id.clone(),
                access: AuthAccess::Write,
            });
            next.run(request).await
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<serde_json::Value> {
                message: "Unauthorized".to_string(),
                code: Some("UNAUTHORIZED"),
                data: None,
            }),
        )
            .into_response(),
    }
}

pub(crate) async fn rate_limit(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }

    let key = extract_client_key(&headers);

    match state.query_limiter.check_key(&key) {
        Ok(_) => {
            state.rate_limit_stats.record_allowed();
            next.run(request).await
        }
        Err(not_until) => {
            let masked = mask_client_key(&key);
            state.rate_limit_stats.record_rejected(&masked);
            let wait =
                not_until.wait_time_from(governor::clock::Clock::now(state.query_limiter.clock()));
            let retry_after_secs = wait.as_secs().max(1);
            let reset_at = Utc::now() + chrono::Duration::seconds(retry_after_secs as i64);

            let body = serde_json::json!({
                "message": "Too Many Requests",
                "code": "TOO_MANY_REQUESTS",
                "data": null,
                "meta": {
                    "rate_limit": {
                        "remaining": 0,
                        "reset_at": reset_at.to_rfc3339(),
                    }
                }
            });

            (
                StatusCode::TOO_MANY_REQUESTS,
                [(
                    header::RETRY_AFTER,
                    axum::http::HeaderValue::from_str(&retry_after_secs.to_string())
                        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("1")),
                )],
                Json(body),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn header(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(value).expect("valid header"),
        );
        headers
    }

    #[test]
    fn extract_bearer_token() {
        let headers = header("Bearer secret-token");
        match extract_auth(&headers).unwrap() {
            AuthHeader::Bearer(token) => assert_eq!(token, "secret-token"),
            _ => panic!("expected bearer"),
        }
    }

    #[test]
    fn extract_basic_credentials() {
        let encoded = BASE64_STANDARD.encode("pk-test:sk-test");
        let headers = header(&format!("Basic {encoded}"));
        match extract_auth(&headers).unwrap() {
            AuthHeader::Basic { username, password } => {
                assert_eq!(username, "pk-test");
                assert_eq!(password, "sk-test");
            }
            _ => panic!("expected basic"),
        }
    }

    #[test]
    fn extract_auth_rejects_missing_header() {
        assert!(extract_auth(&HeaderMap::new()).is_err());
    }

    #[test]
    fn extract_client_key_masks_bearer_prefix() {
        let headers = header("Bearer very-long-secret-token");
        let key = extract_client_key(&headers);
        assert!(key.starts_with("bearer:very-lon"));
    }
}
