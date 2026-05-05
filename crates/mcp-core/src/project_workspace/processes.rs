use super::*;

pub async fn get_sandbox_config_api(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    load_project_context(&state.db, project_id, user_id).await?;
    let cfg = crate::sandbox::load_project_sandbox_config(&state.db, project_id).await;
    Ok(Json(serde_json::to_value(&cfg).unwrap_or_default()))
}

pub async fn set_sandbox_config_api(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<crate::sandbox::ProjectSandboxConfig>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    load_project_context(&state.db, project_id, user_id).await?;
    crate::sandbox::save_project_sandbox_config(&state.db, project_id, &body).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn stop_agent_process(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id_str, process_id_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let process_id = Uuid::parse_str(&process_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Process id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    match crate::agent_processes::stop_process(&state.db, process_id).await {
        Ok(msg) => Ok(Json(json!({ "ok": true, "message": msg }))),
        Err(e) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// POST /api/projects/:id/agent-processes/clear-finished
/// Elimina dal DB tutti i processi stopped/failed del progetto
pub async fn clear_finished_processes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id_str): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let result = sqlx::query(
        "DELETE FROM agent_processes WHERE project_id = $1 AND status IN ('stopped', 'failed')"
    )
    .bind(project_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true, "deleted": result.rows_affected() })))
}

/// GET /api/projects/:id/agent-processes/:process_id/stream
/// Server-Sent Events: trasmette l'output di un agent process in tempo reale.
/// Invia prima uno snapshot completo, poi solo le nuove righe ogni 400ms.
/// Chiude lo stream quando il processo termina (status stopped/failed).
pub async fn stream_agent_process_logs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id_str, process_id_str)): AxumPath<(String, String)>,
) -> axum::response::Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use axum::response::IntoResponse;
    use futures::stream;
    use std::convert::Infallible;

    let user_id = match parse_user_id(&claims) {
        Ok(id) => id,
        Err(_) => return (StatusCode::UNAUTHORIZED, "Non autorizzato").into_response(),
    };
    let project_id = match Uuid::parse_str(&project_id_str) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Project id non valido").into_response(),
    };
    let process_id = match Uuid::parse_str(&process_id_str) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "Process id non valido").into_response(),
    };
    // Verifica che il processo appartenga al progetto dell'utente
    if load_project_context(&state.db, project_id, user_id).await.is_err() {
        return (StatusCode::FORBIDDEN, "Accesso negato").into_response();
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM agent_processes WHERE id=$1 AND project_id=$2)"
    )
    .bind(process_id)
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);
    if !exists {
        return (StatusCode::NOT_FOUND, "Processo non trovato").into_response();
    }

    let db = state.db.clone();
    // State: (offset di caratteri già inviati, terminato)
    let sse_stream = stream::unfold(
        (db, process_id, 0usize, false),
        |(db, process_id, sent_offset, done)| async move {
            if done {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            let row: Option<(String, String, String)> = sqlx::query_as(
                "SELECT status, output, error_output FROM agent_processes WHERE id=$1"
            )
            .bind(process_id)
            .fetch_optional(&db)
            .await
            .ok()
            .flatten();

            let (status, output, error_output) = match row {
                Some(r) => r,
                None => return None,
            };
            let full = if error_output.is_empty() {
                output
            } else {
                format!("{}\n--- STDERR ---\n{}", output, error_output)
            };
            let is_done = status == "stopped" || status == "failed";
            let new_text = if full.len() > sent_offset {
                full[sent_offset..].to_string()
            } else {
                String::new()
            };
            let new_offset = full.len();
            let event_data = serde_json::json!({
                "type": if is_done { "end" } else { "data" },
                "text": new_text,
                "status": status,
            });
            let event = Event::default()
                .data(event_data.to_string());
            Some((
                Ok::<Event, Infallible>(event),
                (db, process_id, new_offset, is_done),
            ))
        },
    );

    Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
