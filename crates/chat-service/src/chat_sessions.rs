use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use nexus_auth::Claims;
use nexus_types::{api_error, parse_user_id, ApiResult};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    pub project_id: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub project_id: String,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RenameSessionRequest {
    pub title: String,
}

/// GET /api/chat/sessions
pub async fn list_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ListSessionsQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let limit = params.limit.unwrap_or(50);
    let offset = params.offset.unwrap_or(0);

    let rows = if let Some(pid) = &params.project_id {
        let project_id = Uuid::parse_str(pid)
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
        sqlx::query(
            "SELECT id, project_id, title, created_at, updated_at FROM chat_sessions WHERE user_id = $1 AND project_id = $2 ORDER BY updated_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(user_id).bind(project_id).bind(limit).bind(offset)
        .fetch_all(&state.db).await
    } else {
        sqlx::query(
            "SELECT id, project_id, title, created_at, updated_at FROM chat_sessions WHERE user_id = $1 ORDER BY updated_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(user_id).bind(limit).bind(offset)
        .fetch_all(&state.db).await
    }
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let sessions: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id").to_string(),
        "project_id": r.get::<Uuid, _>("project_id").to_string(),
        "title": r.get::<Option<String>, _>("title"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
    })).collect();

    Ok(Json(json!({ "sessions": sessions })))
}

/// POST /api/chat/sessions
pub async fn create_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateSessionRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&req.project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let session_id = Uuid::new_v4();
    let title = req.title.unwrap_or_else(|| "Nuova conversazione".to_string());

    sqlx::query("INSERT INTO chat_sessions (id, user_id, project_id, title) VALUES ($1, $2, $3, $4)")
        .bind(session_id).bind(user_id).bind(project_id).bind(&title)
        .execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "session": {
            "id": session_id.to_string(),
            "projectId": project_id.to_string(),
            "title": title,
            "status": "active",
        }
    })))
}

/// PATCH /api/chat/sessions/:id
pub async fn rename_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<RenameSessionRequest>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    sqlx::query("UPDATE chat_sessions SET title = $1, updated_at = NOW() WHERE id = $2")
        .bind(&req.title).bind(session_id)
        .execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

/// DELETE /api/chat/sessions/:id
pub async fn delete_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    sqlx::query("DELETE FROM chat_sessions WHERE id = $1")
        .bind(session_id).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/chat/sessions/:id/compact
pub async fn compact_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    // Count messages before compaction
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_messages WHERE session_id = $1")
        .bind(session_id).fetch_one(&state.db).await.unwrap_or(0);

    // Mark old messages as compacted (keep last 10)
    sqlx::query(
        "UPDATE chat_messages SET is_compacted = TRUE WHERE session_id = $1 AND id NOT IN (SELECT id FROM chat_messages WHERE session_id = $1 ORDER BY created_at DESC LIMIT 10)"
    )
    .bind(session_id).execute(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "compacted": true, "total_messages": count })))
}

/// GET /api/projects/:id/memories
pub async fn list_project_memories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let rows = sqlx::query(
        "SELECT id, content, is_active, created_at FROM project_memories WHERE project_id = $1 ORDER BY created_at DESC"
    )
    .bind(project_id).fetch_all(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let memories: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id").to_string(),
        "content": r.get::<String, _>("content"),
        "is_active": r.get::<bool, _>("is_active"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })).collect();

    Ok(Json(json!({ "memories": memories })))
}

/// PATCH /api/memories/:id/toggle
pub async fn toggle_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let memory_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Memory id non valido"))?;

    sqlx::query("UPDATE project_memories SET is_active = NOT is_active WHERE id = $1")
        .bind(memory_id).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}
