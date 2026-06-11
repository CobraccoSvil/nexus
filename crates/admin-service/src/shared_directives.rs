use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct SharedDirective {
    pub key: String,
    pub content: String,
    pub scope: String,
    pub priority: i32,
    pub is_active: bool,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDirectiveRequest {
    pub key: String,
    pub content: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_scope() -> String {
    "agent".to_string()
}
fn default_priority() -> i32 {
    100
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDirectiveRequest {
    pub content: Option<String>,
    pub scope: Option<String>,
    pub priority: Option<i32>,
    pub is_active: Option<bool>,
    pub description: Option<String>,
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

/// SELECT per chiave con mapping 500/404 (punto unico, regola L):
/// prima duplicato identico in `get_directive` e `update_directive`.
async fn fetch_directive_or_404(
    db: &sqlx::PgPool,
    key: &str,
) -> Result<SharedDirective, (StatusCode, Json<Value>)> {
    sqlx::query_as::<_, SharedDirective>(
        "SELECT key, content, scope, priority, is_active, description, created_at, updated_at \
         FROM nexus_shared_directives WHERE key = $1",
    )
    .bind(key)
    .fetch_optional(db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Direttiva '{}' non trovata", key) })),
        )
    })
}

/// GET /api/admin/shared-directives
pub async fn list_directives(State(state): State<AppState>) -> ApiResult {
    let rows = sqlx::query_as::<_, SharedDirective>(
        "SELECT key, content, scope, priority, is_active, description, created_at, updated_at \
         FROM nexus_shared_directives ORDER BY priority ASC, key ASC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({
        "directives": rows,
        "total": rows.len()
    })))
}

/// GET /api/admin/shared-directives/:key
pub async fn get_directive(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult {
    let row = fetch_directive_or_404(&state.db, &key).await?;

    Ok(Json(json!(row)))
}

/// POST /api/admin/shared-directives
pub async fn create_directive(
    State(state): State<AppState>,
    Json(body): Json<CreateDirectiveRequest>,
) -> ApiResult {
    let key = body.key.trim().to_string();
    if key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Chiave vuota" })),
        ));
    }
    if body.content.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Contenuto vuoto" })),
        ));
    }
    let valid_scopes = ["agent", "system", "all"];
    if !valid_scopes.contains(&body.scope.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Scope non valido: '{}'. Valori ammessi: agent, system, all", body.scope) })),
        ));
    }

    let row = sqlx::query_as::<_, SharedDirective>(
        "INSERT INTO nexus_shared_directives (key, content, scope, priority, description) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING key, content, scope, priority, is_active, description, created_at, updated_at",
    )
    .bind(&key)
    .bind(&body.content)
    .bind(&body.scope)
    .bind(body.priority)
    .bind(&body.description)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        let msg = if e.to_string().contains("duplicate") || e.to_string().contains("unique") {
            format!("Direttiva '{}' gia' esistente", key)
        } else {
            e.to_string()
        };
        (StatusCode::CONFLICT, Json(json!({ "error": msg })))
    })?;

    Ok(Json(json!(row)))
}

/// PUT /api/admin/shared-directives/:key
pub async fn update_directive(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<UpdateDirectiveRequest>,
) -> ApiResult {
    let existing = fetch_directive_or_404(&state.db, &key).await?;

    let new_content = body.content.as_deref().unwrap_or(&existing.content).to_string();
    let new_scope = body.scope.as_deref().unwrap_or(&existing.scope).to_string();
    let new_priority = body.priority.unwrap_or(existing.priority);
    let new_active = body.is_active.unwrap_or(existing.is_active);
    let new_desc = body
        .description
        .as_deref()
        .or(existing.description.as_deref())
        .map(|s| s.to_string());

    // Validazione scope
    let valid_scopes = ["agent", "system", "all"];
    if !valid_scopes.contains(&new_scope.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Scope non valido: '{}'. Valori ammessi: agent, system, all", new_scope) })),
        ));
    }

    let row = sqlx::query_as::<_, SharedDirective>(
        "UPDATE nexus_shared_directives \
         SET content = $2, scope = $3, priority = $4, is_active = $5, description = $6, updated_at = NOW() \
         WHERE key = $1 \
         RETURNING key, content, scope, priority, is_active, description, created_at, updated_at",
    )
    .bind(&key)
    .bind(&new_content)
    .bind(&new_scope)
    .bind(new_priority)
    .bind(new_active)
    .bind(&new_desc)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!(row)))
}

/// POST /api/admin/shared-directives/:key/toggle
pub async fn toggle_directive(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult {
    let row = sqlx::query_as::<_, SharedDirective>(
        "UPDATE nexus_shared_directives \
         SET is_active = NOT is_active, updated_at = NOW() \
         WHERE key = $1 \
         RETURNING key, content, scope, priority, is_active, description, created_at, updated_at",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Direttiva '{}' non trovata", key) })),
        )
    })?;

    Ok(Json(json!(row)))
}

/// DELETE /api/admin/shared-directives/:key
pub async fn delete_directive(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult {
    let result =
        sqlx::query("DELETE FROM nexus_shared_directives WHERE key = $1")
            .bind(&key)
            .execute(&state.db)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
            })?;

    if result.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Direttiva '{}' non trovata", key) })),
        ));
    }

    Ok(Json(json!({ "ok": true })))
}
