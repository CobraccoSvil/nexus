use super::*;

pub(crate) async fn run_turn(
    state: &AppState,
    user_id: Uuid,
    session_id: Uuid,
    project_id: Uuid,
    profile_id: String,
    user_content: String,
    request_message_id: Uuid,
    active_files: Vec<String>,
    system_context: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    automation_mode: AutomationMode,
    attachments: Vec<ChatAttachment>,
) -> Result<(ChatMessageView, OrchestratorResult), ApiError> {
    let enriched_message = match system_context {
        Some(ctx) if !ctx.is_empty() => format!("{ctx}\n\n{user_content}"),
        _ => user_content.clone(),
    };
    let orchestrator_output = state
        .orchestrator
        .run(
            &state.db,
            OrchestratorRequest {
                user_id: user_id.to_string(),
                project_id: project_id.to_string(),
                profile_id,
                message: enriched_message,
                active_files,
                session_id: Some(session_id.to_string()),
                request_message_id: Some(request_message_id.to_string()),
                provider_override,
                model_override,
                automation_mode,
                attachments,
            },
        )
        .await
        // E' QUI che la resa moriva: la catena anyhow contiene ancora
        // GatewayHttpError / GatewayTransportError — cioe' status, codice e la
        // frase gia' scritta dal gateway — e `e.to_string()` li appiattiva tutti
        // in una riga tecnica. Da questo punto in poi nessuno poteva piu'
        // ricostruire nulla se non con una regex sulla prosa (regola M).
        .map_err(|e| {
            nexus_types::api_error_rendered(
                StatusCode::BAD_REQUEST,
                &crate::nexus_gateway::rendered_from_error(&e),
            )
        })?;

    let payload = &orchestrator_output.payload;
    let raw_content = payload["completion"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Se il gateway ha re-instradato la richiesta su provider locale per privacy:
    // 1. Azzerare la preferenza di sessione → al prossimo msg si torna al routing automatico
    // 2. Anteporre una nota informativa alla risposta
    let assistant_content = if let Some(pr) = payload["completion"]["privacy_rerouted"].as_object()
    {
        let provider = pr
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("locale");
        let tier = pr.get("blocked_tier").and_then(|v| v.as_u64()).unwrap_or(0);
        // Azzera la preferenza di sessione
        clear_session_preferred_provider_after_privacy(&state.db, session_id).await;
        format!(
            "> **Privacy attiva** — contenuto sensibile rilevato (livello {tier}). \
             Risposta generata dal modello locale `{provider}` per proteggere i tuoi dati.\n\n{}",
            raw_content
        )
    } else {
        raw_content
    };
    let run_id = payload["run_id"].as_str().unwrap_or("").to_string();
    let metadata = json!({
        "provider": payload["provider"].as_str().unwrap_or(""),
        "model": payload["model"].as_str().unwrap_or(""),
        "intent": payload["intent"].as_str().unwrap_or("chat"),
        "runId": run_id,
        "promptTokens": payload["prompt_tokens"].as_i64().unwrap_or(0),
        // Turno singolo (nessun loop agentico): l'ultimo prompt coincide col
        // totale. Alimenta il context ratio della UI (nel path agentico i due
        // valori divergono: vedi agent_run.rs, lastPromptTokens).
        "lastPromptTokens": payload["prompt_tokens"].as_i64().unwrap_or(0),
        "completionTokens": payload["completion_tokens"].as_i64().unwrap_or(0),
        "totalTokens": payload["total_tokens"].as_i64().unwrap_or(0),
        "totalCost": payload["total_cost"].as_f64().unwrap_or(0.0),
        "currency": payload["currency"].as_str().unwrap_or("EUR"),
        "automationMode": payload["automation_mode"].as_str().unwrap_or("confirm"),
    });

    let assistant_id = insert_message(
        &state.db,
        session_id,
        project_id,
        "assistant",
        &assistant_content,
        metadata,
        Some(request_message_id),
    )
    .await?;

    nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::ChatMessageAdded {
            session_id,
            message_id: assistant_id,
            role: "assistant".into(),
            total_tokens: payload["total_tokens"].as_i64(),
            total_cost_usd: payload["total_cost"].as_f64(),
        },
    );

    // separazione DB: chat_sessions e' migrata, instrada sul pool del progetto
    let project_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
    sqlx::query(
        r#"
        UPDATE chat_sessions
        SET updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .execute(&project_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let row = load_message_by_id(&state.db, project_id, assistant_id).await?;
    let view = to_message_view(&row)?;
    Ok((view, orchestrator_output))
}
