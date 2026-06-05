// Comandi terminale SSE stream, presence, ack, finish.

use super::*;

/// Verifica che `user_id` abbia accesso al progetto (presenza in
/// `project_members`). Punto unico (regola L, S54) per il pattern duplicato
/// nei 3+ handler terminale (presence/ack/finish/stream).
async fn ensure_project_membership(
    db: &sqlx::PgPool,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let access = sqlx::query("SELECT role FROM project_members WHERE project_id=$1 AND user_id=$2")
        .bind(project_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if access.is_none() {
        return Err(api_error(StatusCode::FORBIDDEN, "Accesso negato"));
    }
    Ok(())
}

/// Valida `consumer_id` non vuoto. Pattern duplicato in 3 handler.
fn require_consumer_id(consumer_id: &str) -> Result<&str, ApiError> {
    let trimmed = consumer_id.trim();
    if trimmed.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "consumerId obbligatorio"));
    }
    Ok(trimmed)
}

/// SSE stream per inviare comandi ai terminali IDE dell'utente.
pub async fn terminal_commands_stream(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<Uuid>,
    Query(query): Query<TerminalStreamQuery>,
    Extension(claims): Extension<Claims>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>>, ApiError> {
    let user_id = parse_user_id(&claims)?;
    let consumer_id = query
        .consumer_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Verifica accesso al progetto
    let access = sqlx::query("SELECT role FROM project_members WHERE project_id=$1 AND user_id=$2")
        .bind(project_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if access.is_none() {
        return Err(api_error(StatusCode::FORBIDDEN, "Accesso negato"));
    }

    let db = state.db.clone();
    let consumer_for_db = consumer_id.clone();

    // Stream che interroga il DB ogni 500ms per comandi pending
    let stream = futures::stream::unfold(db, move |db| {
        let consumer_for_db = consumer_for_db.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let _ = sqlx::query(
                "UPDATE terminal_commands
             SET status = 'pending', claimed_at = NULL, claimed_by = NULL
             WHERE project_id = $1
               AND status = 'in_flight'
               AND claimed_at < NOW() - INTERVAL '20 seconds'",
            )
            .bind(project_id)
            .execute(&db)
            .await;

            let rows = sqlx::query(
                "UPDATE terminal_commands \
             SET status = 'in_flight', claimed_at = NOW(), claimed_by = $2 \
             WHERE id IN ( \
                 SELECT id FROM terminal_commands \
                 WHERE project_id = $1 AND status = 'pending' \
                 ORDER BY created_at ASC LIMIT 10 \
             ) RETURNING id, command, session_id, created_at",
            )
            .bind(project_id)
            .bind(&consumer_for_db)
            .fetch_all(&db)
            .await
            .unwrap_or_default();

            let events: Vec<Result<Event, std::convert::Infallible>> = rows
                .into_iter()
                .filter_map(|row| {
                    let cmd_id: Uuid = row.try_get("id").ok()?;
                    let command: String = row.try_get("command").ok()?;
                    let session_id: Option<Uuid> = row.try_get("session_id").ok().flatten();
                    let created_at: Option<chrono::DateTime<chrono::Utc>> =
                        row.try_get("created_at").ok();
                    let payload = serde_json::json!({
                        "commandId": cmd_id.to_string(),
                        "command": command,
                        "sessionId": session_id.map(|s| s.to_string()),
                        "createdAt": created_at.map(|value| value.to_rfc3339()),
                    });
                    serde_json::to_string(&payload)
                        .ok()
                        .map(|data| Ok(Event::default().event("terminal_command").data(data)))
                })
                .collect();

            Some((futures::stream::iter(events), db))
        }
    })
    .flatten();

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub async fn terminal_presence(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<Uuid>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<TerminalPresenceRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let _consumer_id = require_consumer_id(&body.consumer_id)?;
    ensure_project_membership(&state.db, user_id, project_id).await?;

    let key = terminal_consumer_key(user_id, project_id);
    if body.connected {
        state
            .terminal_consumers
            .entry(key)
            .and_modify(|value| *value += 1)
            .or_insert(1);
    } else if let Some(mut count) = state.terminal_consumers.get_mut(&key) {
        if *count > 1 {
            *count -= 1;
        } else {
            drop(count);
            state.terminal_consumers.remove(&key);
        }
    }

    Ok(Json(json!({ "ok": true })))
}

pub async fn terminal_command_ack(
    State(state): State<AppState>,
    AxumPath((project_id, command_id)): AxumPath<(Uuid, Uuid)>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<TerminalAckRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let consumer_id = require_consumer_id(&body.consumer_id)?;
    ensure_project_membership(&state.db, user_id, project_id).await?;

    let preview = body.output_preview.map(|value| {
        let mut trimmed = value.trim().to_string();
        if trimmed.chars().count() > 1200 {
            trimmed = trimmed.chars().take(1200).collect();
        }
        trimmed
    });
    let fail_reason = body.error.map(|value| {
        let mut trimmed = value.trim().to_string();
        if trimmed.chars().count() > 500 {
            trimmed = trimmed.chars().take(500).collect();
        }
        trimmed
    });

    let status = if body.delivered {
        "delivered"
    } else {
        "failed"
    };
    let updated = sqlx::query(
        "UPDATE terminal_commands
         SET status = $4,
             delivered_at = CASE WHEN $4 = 'delivered' THEN NOW() ELSE delivered_at END,
             failed_at = CASE WHEN $4 = 'failed' THEN NOW() ELSE failed_at END,
             fail_reason = COALESCE($5, fail_reason),
             output_preview = COALESCE($6, output_preview)
         WHERE id = $1
           AND project_id = $2
           AND status = 'in_flight'
           AND claimed_by = $3",
    )
    .bind(command_id)
    .bind(project_id)
    .bind(consumer_id)
    .bind(status)
    .bind(fail_reason)
    .bind(preview)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if updated.rows_affected() == 0 {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Comando non disponibile per ACK",
        ));
    }

    Ok(Json(json!({
        "ok": true,
        "status": status,
    })))
}

pub async fn terminal_command_finish(
    State(state): State<AppState>,
    AxumPath((project_id, command_id)): AxumPath<(Uuid, Uuid)>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<TerminalFinishRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let _consumer_id = require_consumer_id(&body.consumer_id)?;
    ensure_project_membership(&state.db, user_id, project_id).await?;

    let full_output = {
        let trimmed = body.full_output.trim().to_string();
        if trimmed.chars().count() > 8000 {
            trimmed.chars().take(8000).collect::<String>()
        } else {
            trimmed
        }
    };

    let _ = sqlx::query(
        "UPDATE terminal_commands
         SET finished_at = NOW(),
             exit_code = $3,
             full_output = $4
         WHERE id = $1
           AND project_id = $2",
    )
    .bind(command_id)
    .bind(project_id)
    .bind(body.exit_code)
    .bind(&full_output)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}
