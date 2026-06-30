use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::stream::Stream;
use nexus_auth::Claims;
use nexus_types::{api_error, parse_user_id, ApiResult};
use serde_json::{json, Value};
use sqlx::Row;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::AppState;

/// GET /api/chat/sessions/:id/agent-stream (SSE)
pub async fn agent_stream(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let _user_id = parse_user_id(&claims).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let session_id = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Find active run for this session
    let run_row = sqlx::query("SELECT id FROM agent_runs WHERE session_id = $1 AND status = 'running' ORDER BY created_at DESC LIMIT 1")
        .bind(session_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let run_id: Uuid = match run_row {
        Some(row) => row.get("id"),
        None => return Err(StatusCode::NOT_FOUND),
    };

    // Subscribe to agent channel
    let rx = if let Some(sender) = state.agent_channels.get(&run_id) {
        sender.subscribe()
    } else {
        // Create channel if not exists
        let (tx, rx) = tokio::sync::broadcast::channel(64);
        state.agent_channels.insert(run_id, tx);
        rx
    };

    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(event) => {
                let data = serde_json::to_string(&event).unwrap_or_default();
                Some(Ok(Event::default().event(event.event_type).data(data)))
            }
            Err(_) => None,
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// GET /api/chat/sessions/:session_id/active-run
pub async fn get_active_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let sid = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;

    let row = sqlx::query(
        "SELECT id, status, iteration_count, final_answer, created_at FROM agent_runs WHERE session_id = $1 ORDER BY created_at DESC LIMIT 1"
    )
    .bind(sid).fetch_optional(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match row {
        Some(r) => Ok(Json(json!({
            "id": r.get::<Uuid, _>("id").to_string(),
            "status": r.get::<String, _>("status"),
            "iteration_count": r.get::<i32, _>("iteration_count"),
            "final_answer": r.get::<Option<String>, _>("final_answer"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        }))),
        None => Ok(Json(json!({ "active_run": null }))),
    }
}

/// GET /api/chat/agent-runs/:run_id
pub async fn get_agent_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let rid = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;

    let row = sqlx::query(
        "SELECT id, session_id, status, iteration_count, final_answer, pending_actions_json, created_at FROM agent_runs WHERE id = $1"
    )
    .bind(rid).fetch_optional(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Run non trovato"))?;

    Ok(Json(json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "session_id": row.get::<Uuid, _>("session_id").to_string(),
        "status": row.get::<String, _>("status"),
        "iteration_count": row.get::<i32, _>("iteration_count"),
        "final_answer": row.get::<Option<String>, _>("final_answer"),
        "pending_actions": row.get::<Option<Value>, _>("pending_actions_json"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })))
}

/// POST /api/chat/agent-runs/:run_id/confirm
pub async fn confirm_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let rid = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;

    sqlx::query("UPDATE agent_runs SET status = 'confirmed', pending_actions_json = NULL WHERE id = $1")
        .bind(rid).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Notify agent loop via channel
    if let Some(sender) = state.agent_channels.get(&rid) {
        let _ = sender.send(crate::AgentStepEvent {
            run_id: rid,
            event_type: "confirmed".to_string(),
            data: json!({}),
        });
    }

    Ok(Json(json!({ "confirmed": true })))
}

/// POST /api/chat/agent-runs/:run_id/cancel
pub async fn cancel_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let rid = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;

    sqlx::query("UPDATE agent_runs SET status = 'cancelled' WHERE id = $1")
        .bind(rid).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Notify agent loop
    if let Some(sender) = state.agent_channels.get(&rid) {
        let _ = sender.send(crate::AgentStepEvent {
            run_id: rid,
            event_type: "cancelled".to_string(),
            data: json!({}),
        });
    }

    // Cleanup channel
    state.agent_channels.remove(&rid);

    Ok(Json(json!({ "cancelled": true })))
}
