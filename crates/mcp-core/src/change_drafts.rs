// ═══════════════════════════════════════════════════════════════════════════
// change_drafts.rs — Endpoint per ChangeDrafter agent
//
// Tabella `change_drafts` (mig 0177): registra ogni proposta di modifica
// generata da agenti (autofix, review, sub-agent, ecc.) e mostra all'utente
// per approvazione/rifiuto via UI chat.
//
// Endpoint:
//   POST   /api/change-drafts        — crea draft (chiamato da agenti backend)
//   GET    /api/change-drafts?status=pending&project_id=...
//   GET    /api/change-drafts/:id
//   POST   /api/change-drafts/:id/approve
//   POST   /api/change-drafts/:id/reject
//
// L'applicazione effettiva del diff (apply) e' delegata a un worker
// downstream (vedi ChangeDrafterApplyWorker — step successivo).
// Per ora `approve` solo marca status='approved'. Un trigger esterno
// chiamera' il worker che esegue il diff.
// ═══════════════════════════════════════════════════════════════════════════

use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct CreateDraftBody {
    pub project_id: Option<Uuid>,
    pub trigger_kind: String,    // 'user_chat'|'autofix'|'review'|'manual'|'sub_agent'
    pub summary: String,         // 1-2 sentence summary
    pub draft: Value,            // JSON: { razionale, impact_analysis, diff_proposto, ... }
}

#[derive(Debug, Serialize)]
pub struct DraftSummary {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub trigger_kind: String,
    pub summary: String,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub related_commit_sha: Option<String>,
}

/// `POST /api/change-drafts`
pub async fn create_draft(
    State(state): State<AppState>,
    Json(body): Json<CreateDraftBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let valid_triggers = ["user_chat", "autofix", "review", "manual", "sub_agent"];
    if !valid_triggers.contains(&body.trigger_kind.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("trigger_kind invalido: {}", body.trigger_kind),
        ));
    }
    if body.summary.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "summary obbligatorio".to_string()));
    }

    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO change_drafts (id, project_id, trigger_kind, summary, draft_json, status)
        VALUES ($1, $2, $3, $4, $5, 'pending')
        "#,
    )
    .bind(id)
    .bind(body.project_id)
    .bind(&body.trigger_kind)
    .bind(&body.summary)
    .bind(&body.draft)
    .execute(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB insert: {e}")))?;

    Ok(Json(json!({
        "id": id,
        "status": "pending",
    })))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub project_id: Option<Uuid>,
    pub limit: Option<i64>,
}

/// `GET /api/change-drafts`
pub async fn list_drafts(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let status = q.status.unwrap_or_default();
    let project_id = q.project_id;

    let rows = sqlx::query(
        r#"
        SELECT id, project_id, trigger_kind, summary, status,
               created_at, updated_at, related_commit_sha
        FROM change_drafts
        WHERE ($1 = '' OR status = $1)
          AND ($2::uuid IS NULL OR project_id = $2)
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(&status)
    .bind(project_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB query: {e}")))?;

    let items: Vec<DraftSummary> = rows
        .into_iter()
        .map(|r| DraftSummary {
            id: r.try_get("id").unwrap_or_else(|_| Uuid::nil()),
            project_id: r.try_get("project_id").ok(),
            trigger_kind: r.try_get("trigger_kind").unwrap_or_default(),
            summary: r.try_get("summary").unwrap_or_default(),
            status: r.try_get("status").unwrap_or_default(),
            created_at: r
                .try_get("created_at")
                .unwrap_or_else(|_| chrono::Utc::now()),
            updated_at: r
                .try_get("updated_at")
                .unwrap_or_else(|_| chrono::Utc::now()),
            related_commit_sha: r.try_get("related_commit_sha").ok(),
        })
        .collect();

    Ok(Json(json!({ "items": items })))
}

/// `GET /api/change-drafts/:id`
pub async fn get_draft(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let row = sqlx::query(
        r#"
        SELECT id, project_id, trigger_kind, summary, draft_json, status,
               applied_at, related_commit_sha, created_at, updated_at
        FROM change_drafts WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB: {e}")))?
    .ok_or((StatusCode::NOT_FOUND, "draft non trovato".to_string()))?;

    let draft_json: Value = row.try_get("draft_json").unwrap_or(json!({}));

    Ok(Json(json!({
        "id": id,
        "project_id": row.try_get::<Option<Uuid>, _>("project_id").ok().flatten(),
        "trigger_kind": row.try_get::<String, _>("trigger_kind").unwrap_or_default(),
        "summary": row.try_get::<String, _>("summary").unwrap_or_default(),
        "draft": draft_json,
        "status": row.try_get::<String, _>("status").unwrap_or_default(),
        "applied_at": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("applied_at").ok().flatten(),
        "related_commit_sha": row.try_get::<Option<String>, _>("related_commit_sha").ok().flatten(),
        "created_at": row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at").ok(),
        "updated_at": row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at").ok(),
    })))
}

/// `POST /api/change-drafts/:id/approve` — marca draft come approvato.
/// L'apply effettivo del diff e' delegato a un worker (step successivo).
pub async fn approve_draft(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let affected = sqlx::query(
        "UPDATE change_drafts SET status = 'approved', updated_at = NOW() WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB: {e}")))?
    .rows_affected();

    if affected == 0 {
        return Err((
            StatusCode::CONFLICT,
            "draft non in stato 'pending' o inesistente".to_string(),
        ));
    }

    Ok(Json(json!({
        "id": id,
        "status": "approved",
        "message": "Draft approvato. L'applicazione effettiva del diff e' delegata al worker."
    })))
}

/// `POST /api/change-drafts/:id/reject`
pub async fn reject_draft(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let affected = sqlx::query(
        "UPDATE change_drafts SET status = 'rejected', updated_at = NOW() WHERE id = $1 AND status IN ('pending', 'approved')",
    )
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB: {e}")))?
    .rows_affected();

    if affected == 0 {
        return Err((
            StatusCode::CONFLICT,
            "draft non rifiutabile in questo stato".to_string(),
        ));
    }

    Ok(Json(json!({
        "id": id,
        "status": "rejected",
    })))
}
