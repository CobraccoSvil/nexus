//! Route admin HTTP per il Sudo Manager (ADR 0017).
//!
//! Tutti gli endpoint qui vivono sotto `/api/admin/sudo/*` e richiedono
//! middleware `require_admin`. Esposti:
//!
//!   GET    /api/admin/sudo/status              — stato installazione
//!   GET    /api/admin/sudo/purposes             — lista purposes
//!   POST   /api/admin/sudo/purposes             — crea purpose
//!   PATCH  /api/admin/sudo/purposes/:id         — modifica (enabled, descr, ecc.)
//!   DELETE /api/admin/sudo/purposes/:id         — rimuove purpose
//!   POST   /api/admin/sudo/execute              — esegue purpose
//!   GET    /api/admin/sudo/audit                — audit log (paginato)

use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use nexus_types::{api_error, ApiResult};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

// ─────────────────────────────── Status ──────────────────────────────────

pub async fn admin_sudo_status(State(state): State<AppState>) -> ApiResult {
    let s = crate::sudo_manager::status(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::to_value(&s).unwrap_or_else(|_| json!({}))))
}

// ─────────────────────────────── Purposes ────────────────────────────────

pub async fn admin_sudo_list_purposes(State(state): State<AppState>) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT id, name, description, command_template, requires_confirm,
               enabled, category, created_by, created_at, updated_at
        FROM nexus_sudo_purposes
        ORDER BY category, name
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use sqlx::Row;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "description": r.try_get::<String, _>("description").unwrap_or_default(),
                "command_template": r.try_get::<String, _>("command_template").unwrap_or_default(),
                "requires_confirm": r.try_get::<bool, _>("requires_confirm").unwrap_or(true),
                "enabled": r.try_get::<bool, _>("enabled").unwrap_or(false),
                "category": r.try_get::<String, _>("category").unwrap_or_default(),
                "created_by": r.try_get::<String, _>("created_by").unwrap_or_default(),
                "created_at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
                    .ok().map(|t| t.to_rfc3339()),
                "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
                    .ok().map(|t| t.to_rfc3339()),
            })
        })
        .collect();
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}

#[derive(Debug, Deserialize)]
pub struct CreatePurposeBody {
    pub name: String,
    pub description: String,
    pub command_template: String,
    #[serde(default = "default_true")]
    pub requires_confirm: bool,
    #[serde(default = "default_category")]
    pub category: String,
}
fn default_true() -> bool {
    true
}
fn default_category() -> String {
    "general".to_string()
}

pub async fn admin_sudo_create_purpose(
    State(state): State<AppState>,
    axum::Extension(claims): axum::Extension<crate::auth::Claims>,
    Json(body): Json<CreatePurposeBody>,
) -> ApiResult {
    let res = sqlx::query(
        r#"
        INSERT INTO nexus_sudo_purposes
            (name, description, command_template, requires_confirm, category, created_by)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#,
    )
    .bind(&body.name)
    .bind(&body.description)
    .bind(&body.command_template)
    .bind(body.requires_confirm)
    .bind(&body.category)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await;
    match res {
        Ok(row) => {
            use sqlx::Row;
            let id: Uuid = row.try_get("id").unwrap_or_else(|_| Uuid::nil());
            Ok(Json(json!({ "ok": true, "id": id.to_string() })))
        }
        Err(e) => {
            // Distingui violazione CHECK (formato name/comando) da duplicato
            let msg = e.to_string();
            let status = if msg.contains("duplicate key") {
                StatusCode::CONFLICT
            } else if msg.contains("nexus_sudo_purposes_name_format")
                || msg.contains("nexus_sudo_purposes_command_safe")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            Err(api_error(status, msg))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PatchPurposeBody {
    pub description: Option<String>,
    pub command_template: Option<String>,
    pub requires_confirm: Option<bool>,
    pub enabled: Option<bool>,
    pub category: Option<String>,
}

pub async fn admin_sudo_patch_purpose(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchPurposeBody>,
) -> ApiResult {
    let res = sqlx::query(
        r#"
        UPDATE nexus_sudo_purposes SET
            description       = COALESCE($1, description),
            command_template  = COALESCE($2, command_template),
            requires_confirm  = COALESCE($3, requires_confirm),
            enabled           = COALESCE($4, enabled),
            category          = COALESCE($5, category),
            updated_at        = NOW()
        WHERE id = $6
        "#,
    )
    .bind(body.description.as_deref())
    .bind(body.command_template.as_deref())
    .bind(body.requires_confirm)
    .bind(body.enabled)
    .bind(body.category.as_deref())
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    if res.rows_affected() == 0 {
        return Err(api_error(StatusCode::NOT_FOUND, "purpose non trovato"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn admin_sudo_delete_purpose(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let res = sqlx::query("DELETE FROM nexus_sudo_purposes WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if res.rows_affected() == 0 {
        return Err(api_error(StatusCode::NOT_FOUND, "purpose non trovato"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ─────────────────────────────── Execute ─────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExecuteBody {
    pub purpose: String,
}

pub async fn admin_sudo_execute(
    State(state): State<AppState>,
    Json(body): Json<ExecuteBody>,
) -> ApiResult {
    let outcome = crate::sudo_manager::execute(&state.db, &body.purpose)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(json!({
        "ok": outcome.success,
        "purpose": outcome.purpose,
        "exit_code": outcome.exit_code,
        "duration_ms": outcome.duration_ms,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
    })))
}

// ─────────────────────────────── Audit ───────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    pub purpose: Option<String>,
}
fn default_limit() -> i64 {
    50
}

pub async fn admin_sudo_audit(
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> ApiResult {
    let limit = q.limit.clamp(1, 500);
    let rows = if let Some(purpose) = q.purpose.as_deref() {
        sqlx::query(
            r#"
            SELECT id, purpose_name, full_command, requested_by_service, exit_code,
                   duration_ms, executed_at
            FROM nexus_sudo_audit_log
            WHERE purpose_name = $1
            ORDER BY executed_at DESC LIMIT $2
            "#,
        )
        .bind(purpose)
        .bind(limit)
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query(
            r#"
            SELECT id, purpose_name, full_command, requested_by_service, exit_code,
                   duration_ms, executed_at
            FROM nexus_sudo_audit_log
            ORDER BY executed_at DESC LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&state.db)
        .await
    }
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use sqlx::Row;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
                "purpose_name": r.try_get::<String, _>("purpose_name").unwrap_or_default(),
                "full_command": r.try_get::<String, _>("full_command").unwrap_or_default(),
                "requested_by_service": r.try_get::<Option<String>, _>("requested_by_service").unwrap_or_default(),
                "exit_code": r.try_get::<Option<i32>, _>("exit_code").unwrap_or_default(),
                "duration_ms": r.try_get::<Option<i32>, _>("duration_ms").unwrap_or_default(),
                "executed_at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("executed_at")
                    .ok().map(|t| t.to_rfc3339()),
            })
        })
        .collect();
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}
