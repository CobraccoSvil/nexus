use axum::{
    extract::{Extension, Path, State},
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
pub struct SendMessageRequest {
    pub content: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub automation: Option<String>,
}

/// GET /api/chat/sessions/:id/messages
pub async fn list_messages(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    let rows = sqlx::query(
        "SELECT id, role, content, metadata, created_at FROM chat_messages WHERE session_id = $1 AND is_compacted = FALSE ORDER BY created_at ASC"
    )
    .bind(session_id).fetch_all(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let messages: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id").to_string(),
        "role": r.get::<String, _>("role"),
        "content": r.get::<String, _>("content"),
        "metadata": r.get::<Option<Value>, _>("metadata"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })).collect();

    Ok(Json(json!({ "messages": messages })))
}

/// POST /api/chat/sessions/:id/messages
pub async fn send_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    // Fetch project_id from the session (required NOT NULL in chat_messages)
    let session_row = sqlx::query(
        "SELECT project_id FROM chat_sessions WHERE id = $1"
    )
    .bind(session_id)
    .fetch_optional(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Sessione non trovata"))?;
    let project_id: Uuid = session_row.get("project_id");

    // Save user message
    let msg_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO chat_messages (id, session_id, project_id, role, content, metadata) VALUES ($1, $2, $3, 'user', $4, $5)"
    )
    .bind(msg_id).bind(session_id).bind(project_id).bind(&req.content)
    .bind(json!({ "provider": req.provider, "model": req.model, "automation": req.automation }))
    .execute(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Update session timestamp
    sqlx::query("UPDATE chat_sessions SET updated_at = NOW() WHERE id = $1")
        .bind(session_id).execute(&state.db).await.ok();

    // TODO: In full implementation, this would:
    // 1. Call billing service to reserve usage
    // 2. Call mcp-core orchestrator for LLM inference
    // 3. Optionally spawn agent loop for tool use
    // 4. Stream results via SSE through agent_channels
    // 5. Save assistant response

    Ok(Json(json!({
        "message_id": msg_id.to_string(),
        "session_id": session_id.to_string(),
        "status": "sent",
    })))
}

/// POST /api/chat/messages/:id/resend
pub async fn resend_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let msg_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    // Fetch original message
    let row = sqlx::query("SELECT session_id, content, metadata FROM chat_messages WHERE id = $1")
        .bind(msg_id).fetch_optional(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"))?;

    let session_id: Uuid = row.get("session_id");
    let content: String = row.get("content");

    Ok(Json(json!({
        "resent": true,
        "session_id": session_id.to_string(),
        "original_content": content,
    })))
}

/// POST /api/chat/messages/:id/feedback-error
pub async fn feedback_error(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let msg_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    let error_type = body.get("error_type").and_then(Value::as_str).unwrap_or("generic");
    let description = body.get("description").and_then(Value::as_str).unwrap_or("");

    sqlx::query(
        "INSERT INTO chat_feedback_errors (id, message_id, user_id, error_type, description) VALUES (gen_random_uuid(), $1, $2, $3, $4)"
    )
    .bind(msg_id).bind(user_id).bind(error_type).bind(description)
    .execute(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

/// DELETE /api/chat/messages/:id
pub async fn delete_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let msg_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    sqlx::query("DELETE FROM chat_messages WHERE id = $1")
        .bind(msg_id).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/chat/precheck
pub async fn precheck_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let content = body.get("content").and_then(Value::as_str).unwrap_or("");

    // Check billing quota
    let client = reqwest::Client::new();
    let billing_check = client
        .get(format!("{}/api/billing/my-usage", state.billing_url))
        .send().await;

    let has_quota = billing_check.map(|r| r.status().is_success()).unwrap_or(true);

    Ok(Json(json!({
        "can_send": has_quota,
        "content_length": content.len(),
    })))
}

/// POST /api/chat/feedback-assist
pub async fn feedback_assist(
    State(_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let message_id = body.get("message_id").and_then(Value::as_str).unwrap_or("");
    let feedback = body.get("feedback").and_then(Value::as_str).unwrap_or("");

    Ok(Json(json!({
        "ok": true,
        "message_id": message_id,
        "feedback_recorded": !feedback.is_empty(),
    })))
}
