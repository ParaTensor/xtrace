use std::{collections::HashMap, sync::Arc};

use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    Json,
};

use crate::http::common::ApiResponse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthAccess {
    Read,
    Write,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub project_id: Arc<str>,
    pub access: AuthAccess,
}

#[derive(Debug, Clone)]
pub struct AuthRegistry {
    pub default_project_id: Arc<str>,
    bearer_tokens: HashMap<String, AuthContext>,
    basic_credentials: HashMap<(String, String), AuthContext>,
}

impl AuthRegistry {
    pub fn has_basic_auth_configured(&self) -> bool {
        !self.basic_credentials.is_empty()
    }

    pub fn from_config(
        default_project_id: String,
        api_bearer_token: String,
        api_read_bearer_token: Option<String>,
        langfuse_public_key: Option<String>,
        langfuse_secret_key: Option<String>,
        project_tokens_raw: Option<&str>,
        project_basic_raw: Option<&str>,
    ) -> Self {
        let default_project_id = Arc::<str>::from(default_project_id);
        let mut bearer_tokens = HashMap::new();
        let mut basic_credentials = HashMap::new();

        if let Some(raw) = project_tokens_raw {
            for entry in raw.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = entry.splitn(3, ':').collect();
                if parts.len() < 2 {
                    continue;
                }
                let project_id = Arc::<str>::from(parts[0].to_string());
                let write_token = parts[1].to_string();
                bearer_tokens.insert(
                    write_token.clone(),
                    AuthContext {
                        project_id: project_id.clone(),
                        access: AuthAccess::Write,
                    },
                );
                if parts.len() == 3 {
                    let read_token = parts[2].to_string();
                    bearer_tokens.insert(
                        read_token,
                        AuthContext {
                            project_id,
                            access: AuthAccess::Read,
                        },
                    );
                }
            }
        }

        if bearer_tokens.is_empty() {
            bearer_tokens.insert(
                api_bearer_token.clone(),
                AuthContext {
                    project_id: default_project_id.clone(),
                    access: AuthAccess::Write,
                },
            );
            if let Some(read_token) = api_read_bearer_token {
                bearer_tokens.insert(
                    read_token,
                    AuthContext {
                        project_id: default_project_id.clone(),
                        access: AuthAccess::Read,
                    },
                );
            }
        }

        if let Some(raw) = project_basic_raw {
            for entry in raw.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = entry.splitn(3, ':').collect();
                if parts.len() != 3 {
                    continue;
                }
                let project_id = Arc::<str>::from(parts[0].to_string());
                basic_credentials.insert(
                    (parts[1].to_string(), parts[2].to_string()),
                    AuthContext {
                        project_id,
                        access: AuthAccess::Write,
                    },
                );
            }
        }

        if basic_credentials.is_empty() {
            if let (Some(public), Some(secret)) = (langfuse_public_key, langfuse_secret_key) {
                basic_credentials.insert(
                    (public, secret),
                    AuthContext {
                        project_id: default_project_id.clone(),
                        access: AuthAccess::Write,
                    },
                );
            }
        }

        Self {
            default_project_id,
            bearer_tokens,
            basic_credentials,
        }
    }

    pub fn resolve_bearer(&self, token: &str) -> Option<AuthContext> {
        self.bearer_tokens.get(token).cloned()
    }

    pub fn resolve_basic(&self, username: &str, password: &str) -> Option<AuthContext> {
        self.basic_credentials
            .get(&(username.to_string(), password.to_string()))
            .cloned()
    }
}

pub fn is_write_route(method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    match *method {
        Method::POST => matches!(
            path,
            "/v1/l/batch"
                | "/v1/metrics/batch"
                | "/api/public/otel/v1/traces"
                | "/api/public/otel/v1/metrics"
                | "/api/public/media"
        ),
        Method::PUT => path.ends_with("/upload"),
        Method::PATCH => path.starts_with("/api/public/media/"),
        _ => false,
    }
}

pub struct Authenticated(pub AuthContext);

#[async_trait]
impl<S> FromRequestParts<S> for Authenticated
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<ApiResponse<serde_json::Value>>);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .map(Authenticated)
            .ok_or_else(|| {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(ApiResponse {
                        message: "Unauthorized".to_string(),
                        code: Some("UNAUTHORIZED"),
                        data: None,
                    }),
                )
            })
    }
}

impl Authenticated {
    pub fn project_id(&self) -> &str {
        self.0.project_id.as_ref()
    }

    pub fn require_write(&self) -> Result<(), crate::http::error::ApiError> {
        if self.0.access == AuthAccess::Read {
            return Err(crate::http::error::ApiError::Forbidden(
                "read-only token cannot write".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_tokens_map_to_isolated_projects() {
        let registry = AuthRegistry::from_config(
            "default".to_string(),
            "legacy-write".to_string(),
            None,
            None,
            None,
            Some("team-a:write-a:read-a,team-b:write-b"),
            None,
        );
        let a_write = registry.resolve_bearer("write-a").unwrap();
        assert_eq!(a_write.project_id.as_ref(), "team-a");
        assert_eq!(a_write.access, AuthAccess::Write);
        let a_read = registry.resolve_bearer("read-a").unwrap();
        assert_eq!(a_read.project_id.as_ref(), "team-a");
        assert_eq!(a_read.access, AuthAccess::Read);
        let b = registry.resolve_bearer("write-b").unwrap();
        assert_eq!(b.project_id.as_ref(), "team-b");
        assert!(registry.resolve_bearer("legacy-write").is_none());
    }

    #[test]
    fn legacy_single_token_falls_back_to_default_project() {
        let registry = AuthRegistry::from_config(
            "default".to_string(),
            "write-only".to_string(),
            Some("read-only".to_string()),
            None,
            None,
            None,
            None,
        );
        let write = registry.resolve_bearer("write-only").unwrap();
        assert_eq!(write.project_id.as_ref(), "default");
        assert_eq!(write.access, AuthAccess::Write);
        let read = registry.resolve_bearer("read-only").unwrap();
        assert_eq!(read.access, AuthAccess::Read);
    }
}
