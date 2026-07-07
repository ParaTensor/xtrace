use axum::{http::StatusCode, response::IntoResponse, Json};
use thiserror::Error;

use crate::http::common::ApiResponse;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("too many requests")]
    TooManyRequests,
    #[error("service unavailable")]
    ServiceUnavailable,
    #[error("internal error: {0}")]
    Internal(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match &self {
            ApiError::Sqlx(err) => {
                tracing::error!(error = %err, "sqlx error");
            }
            ApiError::Internal(err) => {
                tracing::error!(error = %err, "internal error");
            }
            _ => {}
        }

        let (status, code, msg) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", m),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", "Not Found".to_string()),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, "FORBIDDEN", m),
            ApiError::TooManyRequests => (
                StatusCode::TOO_MANY_REQUESTS,
                "TOO_MANY_REQUESTS",
                "Too Many Requests".to_string(),
            ),
            ApiError::ServiceUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                "Service Unavailable".to_string(),
            ),
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal Error".to_string(),
            ),
            ApiError::Sqlx(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal Error".to_string(),
            ),
        };

        let body = Json(ApiResponse::<serde_json::Value> {
            message: msg,
            code: Some(code),
            data: None,
        });

        (status, body).into_response()
    }
}
