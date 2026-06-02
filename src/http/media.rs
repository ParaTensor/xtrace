use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    http::error::ApiError,
    media::{
        content_url, media_file_path, media_id_from_sha256_hash, upload_url, url_expiry_rfc3339,
    },
    state::{AppState, DatabaseConnection, MediaRow},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateMediaBody {
    #[allow(dead_code)]
    pub trace_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub observation_id: Option<String>,
    pub content_type: String,
    pub content_length: i64,
    pub sha256_hash: String,
    #[allow(dead_code)]
    pub field: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateMediaResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    upload_url: Option<String>,
    media_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PatchMediaBody {
    pub uploaded_at: DateTime<Utc>,
    pub upload_http_status: i32,
    #[serde(default)]
    #[allow(dead_code)]
    pub upload_http_error: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub upload_time_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GetMediaResponse {
    media_id: String,
    content_type: String,
    content_length: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    uploaded_at: Option<DateTime<Utc>>,
    url: String,
    url_expiry: String,
}

fn public_base_url(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(base) = state.public_base_url.as_deref() {
        return base.to_string();
    }
    if let Some(proto) = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
            return format!("{proto}://{host}");
        }
    }
    if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        return format!("http://{host}");
    }
    "http://127.0.0.1:8742".to_string()
}

async fn find_media_by_sha256(
    state: &AppState,
    project_id: &str,
    sha256_hash: &str,
) -> Result<Option<MediaRow>, ApiError> {
    match &state.db {
        DatabaseConnection::Postgres(pool) => {
            let row: Option<MediaRow> = sqlx::query_as(
                r#"
SELECT id, project_id, content_type, content_length, sha256_hash, uploaded_at, created_at
FROM media
WHERE project_id = $1 AND sha256_hash = $2
LIMIT 1
                "#,
            )
            .bind(project_id)
            .bind(sha256_hash)
            .fetch_optional(pool)
            .await?;
            Ok(row)
        }
        DatabaseConnection::Memory(mem) => Ok(mem
            .media
            .iter()
            .find(|e| e.value().project_id == project_id && e.value().sha256_hash == sha256_hash)
            .map(|e| e.value().clone())),
    }
}

async fn get_media_row(
    state: &AppState,
    project_id: &str,
    media_id: &str,
) -> Result<Option<MediaRow>, ApiError> {
    match &state.db {
        DatabaseConnection::Postgres(pool) => {
            let row: Option<MediaRow> = sqlx::query_as(
                r#"
SELECT id, project_id, content_type, content_length, sha256_hash, uploaded_at, created_at
FROM media
WHERE project_id = $1 AND id = $2
                "#,
            )
            .bind(project_id)
            .bind(media_id)
            .fetch_optional(pool)
            .await?;
            Ok(row)
        }
        DatabaseConnection::Memory(mem) => {
            let key = format!("{project_id}:{media_id}");
            Ok(mem.media.get(&key).map(|e| e.value().clone()))
        }
    }
}

async fn upsert_media_row(state: &AppState, row: &MediaRow) -> Result<(), ApiError> {
    match &state.db {
        DatabaseConnection::Postgres(pool) => {
            sqlx::query(
                r#"
INSERT INTO media (id, project_id, content_type, content_length, sha256_hash, uploaded_at, created_at)
VALUES ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (id, project_id) DO UPDATE SET
  content_type = EXCLUDED.content_type,
  content_length = EXCLUDED.content_length,
  sha256_hash = EXCLUDED.sha256_hash,
  uploaded_at = COALESCE(EXCLUDED.uploaded_at, media.uploaded_at)
                "#,
            )
            .bind(&row.id)
            .bind(&row.project_id)
            .bind(&row.content_type)
            .bind(row.content_length)
            .bind(&row.sha256_hash)
            .bind(row.uploaded_at)
            .bind(row.created_at)
            .execute(pool)
            .await?;
            Ok(())
        }
        DatabaseConnection::Memory(mem) => {
            let key = format!("{}:{}", row.project_id, row.id);
            mem.media.insert(key, row.clone());
            Ok(())
        }
    }
}

pub(crate) async fn post_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateMediaBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.sha256_hash.len() != 44 {
        return Err(ApiError::BadRequest(
            "sha256Hash must be a 44 character base64 encoded SHA-256 hash".into(),
        ));
    }
    if body.content_length <= 0 || body.content_length > state.media_max_content_length as i64 {
        return Err(ApiError::BadRequest(format!(
            "contentLength must be between 1 and {}",
            state.media_max_content_length
        )));
    }

    let project_id = state.default_project_id.as_ref();
    let media_id = media_id_from_sha256_hash(&body.sha256_hash);
    let base = public_base_url(&state, &headers);

    if let Some(existing) = find_media_by_sha256(&state, project_id, &body.sha256_hash).await? {
        if existing.uploaded_at.is_some() {
            return Ok((
                StatusCode::CREATED,
                Json(CreateMediaResponse {
                    media_id: existing.id,
                    upload_url: None,
                }),
            ));
        }
    }

    let now = Utc::now();
    let row = MediaRow {
        id: media_id.clone(),
        project_id: project_id.to_string(),
        content_type: body.content_type.clone(),
        content_length: body.content_length,
        sha256_hash: body.sha256_hash.clone(),
        uploaded_at: None,
        created_at: now,
    };
    upsert_media_row(&state, &row).await?;

    let upload_url = upload_url(&base, &media_id);
    Ok((
        StatusCode::CREATED,
        Json(CreateMediaResponse {
            media_id,
            upload_url: Some(upload_url),
        }),
    ))
}

pub(crate) async fn get_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(media_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = state.default_project_id.as_ref();
    let Some(row) = get_media_row(&state, project_id, &media_id).await? else {
        return Err(ApiError::NotFound);
    };
    if row.uploaded_at.is_none() {
        return Err(ApiError::NotFound);
    }

    let base = public_base_url(&state, &headers);
    Ok((
        StatusCode::OK,
        Json(GetMediaResponse {
            media_id: row.id,
            content_type: row.content_type,
            content_length: row.content_length,
            uploaded_at: row.uploaded_at,
            url: content_url(&base, &media_id),
            url_expiry: url_expiry_rfc3339(1),
        }),
    ))
}

pub(crate) async fn patch_media(
    State(state): State<AppState>,
    Path(media_id): Path<String>,
    Json(body): Json<PatchMediaBody>,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = state.default_project_id.as_ref();
    let Some(mut row) = get_media_row(&state, project_id, &media_id).await? else {
        return Err(ApiError::NotFound);
    };
    if body.upload_http_status >= 200 && body.upload_http_status < 300 {
        row.uploaded_at = Some(body.uploaded_at);
    }
    upsert_media_row(&state, &row).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn put_media_upload(
    State(state): State<AppState>,
    Path(media_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = state.default_project_id.as_ref();
    let Some(row) = get_media_row(&state, project_id, &media_id).await? else {
        return Err(ApiError::NotFound);
    };
    if body.len() as i64 != row.content_length {
        return Err(ApiError::BadRequest(format!(
            "expected {} bytes, got {}",
            row.content_length,
            body.len()
        )));
    }

    let path = media_file_path(
        state.media_dir.as_ref(),
        project_id,
        &media_id,
        &row.content_type,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    std::fs::write(&path, &body).map_err(|e| ApiError::Internal(e.to_string()))?;

    let _ = (headers, body);
    Ok(StatusCode::OK)
}

pub(crate) async fn get_media_content(
    State(state): State<AppState>,
    Path(media_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project_id = state.default_project_id.as_ref();
    let Some(row) = get_media_row(&state, project_id, &media_id).await? else {
        return Err(ApiError::NotFound);
    };
    if row.uploaded_at.is_none() {
        return Err(ApiError::NotFound);
    }

    let path = media_file_path(
        state.media_dir.as_ref(),
        project_id,
        &media_id,
        &row.content_type,
    );
    let bytes = std::fs::read(&path).map_err(|_| ApiError::NotFound)?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, row.content_type)],
        bytes,
    ))
}

pub(crate) async fn load_media_bytes_async(
    state: &AppState,
    project_id: &str,
    media_id: &str,
) -> Option<(String, Vec<u8>)> {
    let row = get_media_row(state, project_id, media_id).await.ok()??;
    row.uploaded_at?;
    let path = media_file_path(
        state.media_dir.as_ref(),
        project_id,
        media_id,
        &row.content_type,
    );
    let bytes = std::fs::read(&path).ok()?;
    Some((row.content_type, bytes))
}
