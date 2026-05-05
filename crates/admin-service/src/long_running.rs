use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct LongRunningPattern {
    pub id: uuid::Uuid,
    pub pattern: String,
    pub description: String,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePatternRequest {
    pub pattern: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePatternRequest {
    pub pattern: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

pub async fn list_patterns(State(state): State<AppState>) -> ApiResult {
    let rows = sqlx::query_as::<_, LongRunningPattern>(
        "SELECT id, pattern, description, enabled, created_at FROM long_running_patterns ORDER BY pattern",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))?;

    Ok(Json(json!(rows)))
}

pub async fn create_pattern(
    State(state): State<AppState>,
    Json(body): Json<CreatePatternRequest>,
) -> ApiResult {
    let pattern = body.pattern.trim().to_string();
    if pattern.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": "pattern vuoto" }))));
    }

    let row = sqlx::query_as::<_, LongRunningPattern>(
        "INSERT INTO long_running_patterns (pattern, description) VALUES ($1, $2) RETURNING id, pattern, description, enabled, created_at",
    )
    .bind(&pattern)
    .bind(&body.description)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        let msg = if e.to_string().contains("duplicate") { format!("Pattern '{}' già esistente", pattern) } else { e.to_string() };
        (StatusCode::CONFLICT, Json(json!({ "error": msg })))
    })?;

    Ok(Json(json!(row)))
}

pub async fn update_pattern(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<UpdatePatternRequest>,
) -> ApiResult {
    let existing = sqlx::query_as::<_, LongRunningPattern>(
        "SELECT id, pattern, description, enabled, created_at FROM long_running_patterns WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))?
    .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({ "error": "Pattern non trovato" }))))?;

    let new_pattern = body.pattern.as_deref().unwrap_or(&existing.pattern).trim().to_string();
    let new_desc = body.description.as_deref().unwrap_or(&existing.description).to_string();
    let new_enabled = body.enabled.unwrap_or(existing.enabled);

    let row = sqlx::query_as::<_, LongRunningPattern>(
        "UPDATE long_running_patterns SET pattern = $2, description = $3, enabled = $4 WHERE id = $1 RETURNING id, pattern, description, enabled, created_at",
    )
    .bind(id).bind(&new_pattern).bind(&new_desc).bind(new_enabled)
    .fetch_one(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))?;

    Ok(Json(json!(row)))
}

pub async fn delete_pattern(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult {
    let result = sqlx::query("DELETE FROM long_running_patterns WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))))?;

    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, Json(json!({ "error": "Pattern non trovato" }))));
    }

    Ok(Json(json!({ "ok": true })))
}

#[allow(dead_code)]
pub async fn load_enabled_patterns(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT pattern FROM long_running_patterns WHERE enabled = TRUE")
        .fetch_all(db)
        .await
        .unwrap_or_default()
}
