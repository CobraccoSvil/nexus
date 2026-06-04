// ═══════════════════════════════════════════════════════════════════════════
// agent_todos_routes.rs — API REST per l'editing dei todo di un run (M15.3)
// ═══════════════════════════════════════════════════════════════════════════
//
// Permette all'utente di modificare manualmente i todo di un piano agente
// (contenuto, stato, priorita', criteri di accettazione). La modifica traccia
// l'autore in `edited_by` e ri-emette gli eventi live TodoUpdated + PlanUpdated
// cosi' la checklist in chat resta sincronizzata. Gated da
// `agent.todos.user_editable` (default true).

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use nexus_types::{api_error, ensure_project_access, parse_user_id, ApiResult};

use crate::{auth::Claims, AppState};

// Stati todo ammessi per l'edit manuale.
const VALID_TODO_STATUS: &[&str] = &[
    "pending",
    "in_progress",
    "completed",
    "blocked",
    "cancelled",
];

#[derive(Deserialize)]
pub struct EditTodoBody {
    pub todo_id: String,
    pub content: Option<String>,
    pub status: Option<String>,
    pub priority: Option<i32>,
    pub acceptance_criteria: Option<String>,
}

/// POST /api/projects/:project_id/agent/todos/:run_id/edit
pub async fn edit_todo(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, run_id)): AxumPath<(String, String)>,
    Json(body): Json<EditTodoBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let run_id = Uuid::parse_str(&run_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;
    let todo_id = Uuid::parse_str(&body.todo_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Todo id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    // Gate: editing manuale dei todo abilitato?
    let editable = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.todos.user_editable' LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|s| {
        !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no"
        )
    })
    .unwrap_or(true);
    if !editable {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Editing manuale dei todo disabilitato (agent.todos.user_editable)",
        ));
    }

    if let Some(ref status) = body.status {
        if !VALID_TODO_STATUS.contains(&status.as_str()) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "Status todo non valido (pending/in_progress/completed/blocked/cancelled)",
            ));
        }
    }

    // SET clause dinamica. edited_by tracciato sempre. $1=todo_id $2=run_id
    // $3=project_id $4=edited_by, i campi opzionali da $5.
    let mut sets = vec![
        "updated_at = NOW()".to_string(),
        "edited_by = $4".to_string(),
    ];
    let mut bind_idx = 5u32;
    if body.content.is_some() {
        sets.push(format!("content = ${bind_idx}"));
        bind_idx += 1;
    }
    if body.status.is_some() {
        sets.push(format!("status = ${bind_idx}"));
        bind_idx += 1;
    }
    if body.priority.is_some() {
        sets.push(format!("priority = ${bind_idx}"));
        bind_idx += 1;
    }
    if body.acceptance_criteria.is_some() {
        sets.push(format!("acceptance_criteria = ${bind_idx}"));
    }

    let sql = format!(
        "UPDATE nexus_agent_todos SET {} \
         WHERE id = $1 AND run_id = $2 AND project_id = $3 \
         RETURNING status",
        sets.join(", ")
    );

    let mut query = sqlx::query_scalar::<_, String>(&sql)
        .bind(todo_id)
        .bind(run_id)
        .bind(project_id)
        .bind(user_id.to_string());
    if let Some(ref content) = body.content {
        query = query.bind(content);
    }
    if let Some(ref status) = body.status {
        query = query.bind(status);
    }
    if let Some(priority) = body.priority {
        query = query.bind(priority);
    }
    if let Some(ref ac) = body.acceptance_criteria {
        query = query.bind(ac);
    }

    let new_status = query
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let new_status = match new_status {
        Some(s) => s,
        None => {
            return Err(api_error(
                StatusCode::NOT_FOUND,
                "Todo non trovato per questo run/progetto",
            ))
        }
    };

    // Eventi live: il todo modificato + l'avanzamento aggregato del piano.
    nexus_events::dispatcher::emit_global(
        project_id,
        nexus_events::event::ProjectEvent::TodoUpdated {
            run_id: run_id.to_string(),
            todo_id: todo_id.to_string(),
            seq: None,
            status: new_status.clone(),
        },
    );
    if let Ok((total, completed)) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE status = 'completed') \
         FROM nexus_agent_todos WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&state.db)
    .await
    {
        nexus_events::dispatcher::emit_global(
            project_id,
            nexus_events::event::ProjectEvent::PlanUpdated {
                run_id: run_id.to_string(),
                total: total as i32,
                completed: completed as i32,
            },
        );
    }

    Ok(Json(
        json!({ "ok": true, "todo_id": todo_id, "status": new_status }),
    ))
}

/// GET /api/internal/agent/backlog/:project_id  (no-auth, chiamato dal brain)
///
/// M15.4 — Restituisce i todo marcati carry_over di run precedenti del progetto
/// ancora aperti, cosi' il planner del run successivo li puo' includere come
/// backlog ereditato (backlog_brief). Ordinati per priorita' poi anzianita'.
pub async fn list_backlog(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let rows = sqlx::query_as::<_, (Uuid, String, String, Option<i32>, Option<Uuid>)>(
        "SELECT id, content, status, priority, origin_run_id \
         FROM nexus_agent_todos \
         WHERE project_id = $1 AND carry_over = true \
           AND status NOT IN ('completed', 'cancelled') \
         ORDER BY priority DESC NULLS LAST, seq ASC \
         LIMIT 50",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, content, status, priority, origin_run_id)| {
            json!({
                "todo_id": id,
                "content": content,
                "status": status,
                "priority": priority,
                "origin_run_id": origin_run_id,
            })
        })
        .collect();

    Ok(Json(json!({ "backlog": items, "count": items.len() })))
}
