//! Tipi DTO e logica handler condivisi per gli endpoint long_running_patterns
//! (regola L / ADR 0026, step S21 + cluster E6). Prima duplicati in
//! crates/admin-service/src/long_running.rs e crates/mcp-core/src/long_running.rs.

use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;

use crate::{api_error, ApiResult};

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

// -- Logica handler condivisa (cluster E6) --
// Gli handler axum nei due crate estraggono State/Path/Json e delegano qui.

pub async fn list_patterns_core(db: &PgPool) -> ApiResult {
    let rows = sqlx::query_as::<_, LongRunningPattern>(
        "SELECT id, pattern, description, enabled, created_at FROM long_running_patterns ORDER BY pattern",
    )
    .fetch_all(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!(rows)))
}

pub async fn create_pattern_core(db: &PgPool, body: CreatePatternRequest) -> ApiResult {
    let pattern = body.pattern.trim().to_string();
    if pattern.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "pattern vuoto"));
    }

    let row = sqlx::query_as::<_, LongRunningPattern>(
        "INSERT INTO long_running_patterns (pattern, description) VALUES ($1, $2) RETURNING id, pattern, description, enabled, created_at",
    )
    .bind(&pattern)
    .bind(&body.description)
    .fetch_one(db)
    .await
    .map_err(|e| {
        let msg = if e.to_string().contains("duplicate") {
            format!("Pattern '{}' già esistente", pattern)
        } else {
            e.to_string()
        };
        api_error(StatusCode::CONFLICT, msg)
    })?;

    Ok(Json(json!(row)))
}

pub async fn update_pattern_core(
    db: &PgPool,
    id: uuid::Uuid,
    body: UpdatePatternRequest,
) -> ApiResult {
    let existing = sqlx::query_as::<_, LongRunningPattern>(
        "SELECT id, pattern, description, enabled, created_at FROM long_running_patterns WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Pattern non trovato"))?;

    let new_pattern = body
        .pattern
        .as_deref()
        .unwrap_or(&existing.pattern)
        .trim()
        .to_string();
    let new_desc = body
        .description
        .as_deref()
        .unwrap_or(&existing.description)
        .to_string();
    let new_enabled = body.enabled.unwrap_or(existing.enabled);

    let row = sqlx::query_as::<_, LongRunningPattern>(
        "UPDATE long_running_patterns SET pattern = $2, description = $3, enabled = $4 WHERE id = $1 RETURNING id, pattern, description, enabled, created_at",
    )
    .bind(id)
    .bind(&new_pattern)
    .bind(&new_desc)
    .bind(new_enabled)
    .fetch_one(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!(row)))
}

pub async fn delete_pattern_core(db: &PgPool, id: uuid::Uuid) -> ApiResult {
    let result = sqlx::query("DELETE FROM long_running_patterns WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(api_error(StatusCode::NOT_FOUND, "Pattern non trovato"));
    }

    Ok(Json(json!({ "ok": true })))
}
