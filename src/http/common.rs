use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

#[derive(Debug, Serialize)]
#[allow(non_snake_case)]
pub struct PageMeta {
    pub page: i64,
    pub limit: i64,
    pub totalItems: i64,
    pub totalPages: i64,
}

#[derive(Debug, Serialize)]
pub struct PagedData<T> {
    pub data: Vec<T>,
    pub meta: PageMeta,
}

pub async fn healthz() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match &state.db {
        crate::state::DatabaseConnection::Postgres(pool) => {
            match sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(pool)
                .await
            {
                Ok(_) => (
                    StatusCode::OK,
                    Json(serde_json::json!({ "status": "ready" })),
                )
                    .into_response(),
                Err(e) => {
                    tracing::error!(error = %e, "readyz: database check failed");
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({ "status": "not_ready" })),
                    )
                        .into_response()
                }
            }
        }
        crate::state::DatabaseConnection::Memory(_) => {
            (
                StatusCode::OK,
                Json(serde_json::json!({ "status": "ready" })),
            )
                .into_response()
        }
    }
}
