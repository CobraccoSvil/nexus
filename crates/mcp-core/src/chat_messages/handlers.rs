use super::*;

pub async fn list_chat_messages(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;
    let context = load_session_context(&state, session_id, user_id).await?;
    // Separazione DB: messaggi + agent_runs vivono nel DB del progetto (il
    // JOIN funziona perche' entrambi sono in <slug>_nexus). project_data_pool
    // e' il punto unico; DB non disponibile -> 503 strutturato (regola M).
    let chat_pool = crate::project_db_routes::project_data_pool(&state, context.project_id).await?;

    let rows = sqlx::query(
        r#"
        SELECT cm.id, cm.session_id, cm.project_id, cm.role, cm.content, cm.metadata,
               cm.request_message_id, cm.deleted_at, cm.created_at,
               ar.id AS run_id, ar.status AS run_status
        FROM chat_messages cm
        -- Ancora del run: `agent_runs.run_message_id` punta al messaggio UTENTE
        -- che ha innescato il run, mai alla risposta. Agganciare l'assistant
        -- passa quindi dal suo `request_message_id` (il messaggio utente a cui
        -- risponde); per lo user vale l'id stesso. Con il JOIN su `cm.id` secco
        -- l'assistant non riceveva NE' `run_id` NE' `run_status`: dopo un
        -- reload `message.runId` era undefined, e message-list.tsx apre con
        -- `if (!message.runId) return null` -- l'intero nastro attivita'
        -- spariva, Consiglio delle Competenze incluso, pur essendo tutto nel DB
        -- (difetto osservato il 20/07: "la sezione consiglio scompare quando si
        -- aggiorna"). Nessun messaggio utente ha `request_message_id`, quindi
        -- il COALESCE non altera l'aggancio gia' funzionante su quel lato.
        LEFT JOIN agent_runs ar ON ar.run_message_id = COALESCE(cm.request_message_id, cm.id)
        WHERE cm.session_id = $1
        ORDER BY cm.created_at ASC
        "#,
    )
    .bind(context.session_id)
    .fetch_all(&chat_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut messages: Vec<Value> = Vec::with_capacity(rows.len());
    for row in &rows {
        let view = to_message_view(row)?;
        messages.push(serde_json::to_value(view).unwrap_or_else(|_| json!({})));
    }

    // ── Carica in batch gli allegati per tutti i messaggi della sessione ──
    // Una sola query con JOIN su chat_messages, poi raggruppiamo per message_id
    // ed iniettiamo l'array `attachments` in ogni elemento di `messages`.
    let attachments_rows = sqlx::query(
        r#"
        SELECT cma.id, cma.message_id, cma.project_id, cma.file_name, cma.file_path,
               cma.mime_type, cma.size_bytes, cma.kind, cma.kb_note_id,
               cma.indexed_at, cma.created_at
        FROM chat_message_attachments cma
        JOIN chat_messages cm ON cm.id = cma.message_id
        WHERE cm.session_id = $1
        ORDER BY cma.created_at ASC
        "#,
    )
    .bind(context.session_id)
    .fetch_all(&chat_pool)
    .await
    .unwrap_or_default();

    let mut attachments_by_msg: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    for row in &attachments_rows {
        let msg_id: Uuid = match row.try_get("message_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let att_id: Uuid = match row.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let project_id: Uuid = match row.try_get("project_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let file_name: String = row.try_get("file_name").unwrap_or_default();
        let file_path: String = row.try_get("file_path").unwrap_or_default();
        let mime_type: String = row.try_get("mime_type").unwrap_or_default();
        let size_bytes: i64 = row.try_get("size_bytes").unwrap_or(0);
        let kind: String = row.try_get("kind").unwrap_or_else(|_| "binary".to_string());
        let kb_note_id: Option<Uuid> = row.try_get("kb_note_id").unwrap_or(None);
        let indexed_at: Option<DateTime<Utc>> = row.try_get("indexed_at").unwrap_or(None);
        let created_at: Option<DateTime<Utc>> = row.try_get("created_at").ok();

        let entry = attachments_by_msg.entry(msg_id.to_string()).or_default();
        entry.push(json!({
            "id": att_id.to_string(),
            "messageId": msg_id.to_string(),
            "projectId": project_id.to_string(),
            "fileName": file_name,
            "filePath": file_path,
            "mimeType": mime_type,
            "sizeBytes": size_bytes,
            "kind": kind,
            "kbNoteId": kb_note_id.map(|v| v.to_string()),
            "indexedAt": indexed_at.map(|v| v.to_rfc3339()),
            "createdAt": created_at.map(|v| v.to_rfc3339()),
        }));
    }

    for msg in messages.iter_mut() {
        let msg_id = msg.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        let attachments = attachments_by_msg.remove(msg_id).unwrap_or_default();
        if let Value::Object(map) = msg {
            map.insert("attachments".to_string(), Value::Array(attachments));
        }
    }

    Ok(Json(json!({
        "sessionId": context.session_id.to_string(),
        "projectId": context.project_id.to_string(),
        "messages": messages
    })))
}
/// Risposta di replay per una POST /messages ritentata dal client con lo
/// stesso `clientMessageId` (idempotenza invio, mig progetto 0008). Il
/// messaggio utente esiste gia': si restituisce quello, allegando l'eventuale
/// run attivo della sessione cosi' il client si riaggancia allo stream SSE
/// invece di restare su uno stato ottimistico mai confermato.
async fn replay_idempotent_send(
    state: &AppState,
    session_pool: &PgPool,
    context: &crate::chat_sessions::SessionContext,
    user_message_id: Uuid,
) -> ApiResult {
    let user_row = load_message_by_id(&state.db, context.project_id, user_message_id).await?;
    let user_message = to_message_view(&user_row)?;

    // Run attivo della sessione (stessa soglia di recency del gate
    // anti-run-concorrente): se presente, e' il run avviato dal primo
    // tentativo di questa stessa POST.
    let active_run: Option<(Uuid, String, Option<String>, Option<String>)> = sqlx::query_as(
        // `awaiting_subagents` (Fase D fan-in): il run e' sospeso-VIVO in attesa dei
        // figli background (gemello di `awaiting_confirmation`), quindi il replay
        // idempotente deve ritrovarlo come run attivo del primo tentativo.
        &format!(
            "SELECT id, status, provider, model FROM agent_runs \
         WHERE session_id = $1 \
           AND status IN ({}) \
           AND created_at > NOW() - INTERVAL '15 minutes' \
         ORDER BY created_at DESC \
         LIMIT 1",
            crate::agent_types::ACTIVE_RUN_STATUS_SQL
        ),
    )
    .bind(context.session_id)
    .fetch_optional(session_pool)
    .await
    .ok()
    .flatten();

    let agent_run = active_run.map(|(run_id, status, provider, model)| {
        json!({
            "runId": run_id.to_string(),
            "status": status,
            "provider": provider.unwrap_or_default(),
            "model": model.unwrap_or_default(),
        })
    });

    // Allegati gia' persistiti dal primo tentativo (punto unico
    // message_attachments_json): li restituiamo nello stesso shape del path
    // normale, cosi' i chip e la proposta di indicizzazione KB non si perdono
    // col retry idempotente.
    let saved_attachments = message_attachments_json(session_pool, user_message_id).await;

    Ok(Json(json!({
        "sessionId": context.session_id.to_string(),
        "userMessage": user_message,
        "agentRun": agent_run,
        "savedAttachments": saved_attachments,
        "idempotentReplay": true,
    })))
}

pub async fn send_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<SendChatMessageRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;
    let context = load_session_context(&state, session_id, user_id).await?;
    // Separazione DB: chat_sessions/agent_runs migrate nel DB del progetto.
    // Risolvo una volta il pool del progetto per sessione e lo riuso per tutte
    // le scritture di questo handler. DB non disponibile -> 503: scrivere il
    // messaggio sul meta lo farebbe "sparire" alla riapertura del DB progetto.
    let session_pool =
        crate::project_db_routes::project_data_pool_by_session_from(&state.db, context.session_id)
            .await?;

    let content = body.content.trim();
    if content.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il contenuto del messaggio e' obbligatorio",
        ));
    }

    // ── Hardening anti-run-concorrente (con stale detection) ─────────────────
    // Una sola generazione agentica VERAMENTE attiva per sessione: senza questa
    // guardia, due POST /messages ravvicinate avviavano due run in parallelo e
    // il secondo rubava lo stream SSE (un messaggio orfano).
    //
    // Fix originale: rifiutavamo se esisteva un run status IN ('running',
    // 'awaiting_confirmation'). Difetto strutturale: un crash/restart del
    // backend lasciava il run in 'running' nel DB per sempre -> la sessione
    // restava bloccata col 409 fino a cleanup manuale. La causa radice e' che
    // 'status=running' non implica "vivo": serve verificare la recency.
    //
    // Fix definitivo: consideriamo attivo solo un run con created_at recente
    // (entro la soglia di "vita massima ragionevole"). I run piu' vecchi sono
    // per definizione stale (un turno reale termina entro pochi minuti) e
    // verranno marcati 'interrupted' dal cleanup di startup (vedi main.rs).
    // Soglia: 15 minuti — copre i turni piu' lunghi visti in produzione con
    // largo margine, ma sblocca la chat se qualcosa e' rimasto sospeso.
    //
    // Fix mig 0388: si esclude anche `generation_ended_at IS NOT NULL`. Nel grafo
    // LangGraph l'ordine terminale e' executor -> reflection -> learner -> END:
    // l'evento end_turn (che libera il pulsante invio nel frontend)
    // e' emesso a fine executor, MA reflection_node fa ancora una chiamata LLM di
    // valutazione (secondi) prima che il run sia finalizzato. In quella finestra
    // il run e' 'running' ma la generazione e' di fatto conclusa: senza questa
    // esclusione l'utente vedeva "la chat sembra libera" e prendeva 409. Un run in
    // awaiting_confirmation NON ha emesso end_turn (generation_ended_at IS NULL) e
    // resta correttamente bloccante (pausa-conferma reale).
    //
    // Persiste la modalita' scelta sulla sessione (mig 0371): i run risvegliati
    // (process_resume, service_observer) la ereditano invece di defaultare a
    // Confirm. Solo quando il body porta un valore esplicito.
    if let Some(m) = body
        .automation_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match AutomationMode::try_parse(Some(m)) {
            Ok(mode) => {
                let _ = sqlx::query("UPDATE chat_sessions SET automation_mode = $1 WHERE id = $2")
                    .bind(mode.as_str())
                    .bind(context.session_id)
                    .execute(&session_pool)
                    .await;
            }
            Err(_) => {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "automation_mode non valido: '{m}'. Valori ammessi: study, confirm, automatic"
                    ),
                ));
            }
        }
    }

    // ── Idempotenza invio (mig progetto 0008) ────────────────────────────────
    // Retry di rete della stessa POST: il client dichiara clientMessageId. Se
    // il messaggio risulta gia' persistito in questa sessione, il tentativo
    // precedente era arrivato al server (si e' persa solo la risposta):
    // replay di userMessage + eventuale run attivo, senza duplicare il
    // messaggio ne' avviare un secondo run. DEVE stare PRIMA del gate
    // anti-run-concorrente: il run avviato dal primo tentativo e' proprio
    // quello che il retry deve ritrovare, non un 409.
    if let Some(client_mid) = body.client_message_id {
        let existing_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM chat_messages WHERE session_id = $1 AND client_message_id = $2",
        )
        .bind(context.session_id)
        .bind(client_mid)
        .fetch_optional(&session_pool)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if let Some(existing_id) = existing_id {
            return replay_idempotent_send(&state, &session_pool, &context, existing_id).await;
        }
    }

    if parse_automation_mode(body.automation_mode.as_deref()) != AutomationMode::Study {
        let active_run: Option<Uuid> = sqlx::query_scalar(
            // `awaiting_subagents` (Fase D fan-in) e' un'interruzione MID-TURN, gemella
            // di `awaiting_confirmation`: il run e' sospeso-vivo in attesa dei figli
            // background e NON ha emesso end_turn (generation_ended_at IS NULL resta
            // vero), quindi deve restare bloccante come pausa-conferma. Senza, un nuovo
            // messaggio sulla sessione col padre sospeso avvierebbe un 2o run parallelo
            // che ruba lo stream SSE.
            &format!(
                "SELECT id FROM agent_runs \
             WHERE session_id = $1 \
               AND status IN ({}) \
               AND created_at > NOW() - INTERVAL '15 minutes' \
               AND generation_ended_at IS NULL \
               AND nexus_agent_type IS DISTINCT FROM 'subagent' \
             LIMIT 1",
                crate::agent_types::ACTIVE_RUN_STATUS_SQL
            ),
        )
        .bind(context.session_id)
        .fetch_optional(&session_pool)
        .await
        .ok()
        .flatten();
        if active_run.is_some() {
            return Err(api_error(
                StatusCode::CONFLICT,
                "Un'operazione e' gia' in corso su questa sessione: attendi il completamento del run prima di inviare un nuovo messaggio.",
            ));
        }
    }

    // RC-4 (regola N): il metadata `automationMode` del messaggio riflette il mode
    // PERSISTITO sulla sessione (pool progetto), non un 'confirm' hardcoded. Se il body
    // portava un mode esplicito e' gia' stato validato+persistito sopra (sez.
    // automation_mode -> UPDATE chat_sessions); se era assente, resta il mode scelto
    // dall'utente. Cosi' un RESEND che rilegge questo metadata eredita il mode reale
    // (prima: body None -> "confirm" hardcoded -> il resend ereditava 'confirm' anche
    // con la sessione su 'automatic').
    let message_automation_mode = read_session_automation_mode(&session_pool, context.session_id)
        .await
        .as_str()
        .to_string();
    let user_message_id = match insert_message_with_client_id(
        &state.db,
        context.session_id,
        context.project_id,
        "user",
        content,
        json!({
            "providerOverride": body.provider_override.clone(),
            "modelOverride": body.model_override.clone(),
            "automationMode": message_automation_mode,
            "attachments": body.attachments.clone(),
            // Marca i messaggi auto-generati dal sistema (es. auto-continuazione).
            // Il frontend filtra questi messaggi dalla UI per non confondere l'utente
            // facendogli credere di averli scritti lui.
            "synthetic": body.synthetic,
        }),
        None,
        body.client_message_id,
    )
    .await?
    {
        ClientIdInsert::Inserted(id) => id,
        ClientIdInsert::Duplicate => {
            // Race stretta: un retry concorrente ha vinto l'INSERT tra il
            // pre-check di idempotenza e questo punto. Il segnale e' l'unique
            // violation strutturata (23505, regola M): si rilegge il messaggio
            // vincente e si fa replay, mai duplicare.
            let existing_id: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM chat_messages WHERE session_id = $1 AND client_message_id = $2",
            )
            .bind(context.session_id)
            .bind(body.client_message_id)
            .fetch_optional(&session_pool)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let Some(existing_id) = existing_id else {
                return Err(api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "messaggio duplicato non ritrovato dopo unique violation",
                ));
            };
            return replay_idempotent_send(&state, &session_pool, &context, existing_id).await;
        }
    };
    let user_row = load_message_by_id(&state.db, context.project_id, user_message_id).await?;
    let user_message = to_message_view(&user_row)?;

    // ── Persistenza allegati su filesystem + DB ──────────────────────────────
    // Gli allegati vengono salvati subito dopo l'INSERT del messaggio user, cosi'
    // tutti i return path successivi (model_reset, model_switch, resume, DLP,
    // agent_run, run_turn) restituiscono `savedAttachments` al frontend.
    // Errori filesystem singoli vengono loggati come WARN ma NON bloccano il
    // turno: l'utente riceve la lista degli allegati effettivamente persistiti.
    // Estraggo `saved_attachments_list` come variabile esplicita per riusarlo
    // nei successivi `enrich_attachments_with_ids` (necessari per popolare gli
    // UUID nel blocco <allegati> del prompt iniziale).
    let mut saved_attachments_list: Vec<crate::chat_attachments::SavedAttachment> = Vec::new();
    let saved_attachments_json: Value = if body.attachments.is_empty() {
        json!([])
    } else {
        match crate::projects::load_project_context(&state.db, context.project_id, user_id).await {
            Ok(project_ctx) => {
                let saved = crate::chat_attachments::persist_message_attachments(
                    &state.db,
                    &project_ctx.repository_root_path,
                    context.project_id,
                    user_message_id,
                    &body.attachments,
                )
                .await;
                let json_view = crate::chat_attachments::attachments_to_json(&saved);
                saved_attachments_list = saved;
                json_view
            }
            Err(e) => {
                tracing::warn!(
                    project_id = %context.project_id,
                    message_id = %user_message_id,
                    "load_project_context fallito durante persistenza allegati: {}",
                    e.1["error"].as_str().unwrap_or("errore sconosciuto")
                );
                json!([])
            }
        }
    };

    spawn_embed_conversation_turn(
        state.orchestrator.neural.clone(),
        state.db.clone(),
        context.session_id,
        user_message_id,
        "user".to_string(),
        content.to_string(),
    );

    // ── Hook: auto-creazione nota Knowledge Base ───────────────────────────
    // ADR 0017 v2 TODO 6: il worker `wiki::chat_note_worker` (avviato in
    // main.rs, intervallo 30s default) scansiona periodicamente i messaggi
    // utente e li ingesta in `wiki_docs` (scope=project, kind='chat_note').
    // Il flag `chat_messages.kb_ingested` (mig 0303) garantisce idempotenza.
    // Niente inline qui per non rallentare il path di risposta chat.

    // ── Rilevamento cambio modello esplicito ────────────────────────────────
    // Se il messaggio è un comando "usa mistral / cambia a claude / ecc." e
    // il client non ha già impostato un override manuale, gestiamo lo switch
    // automaticamente: salviamo la preferenza nella sessione e rispondiamo con
    // un messaggio di conferma senza coinvolgere l'AI.
    if body.provider_override.is_none() {
        // Reset al routing automatico
        if detect_model_reset(content) {
            let _ = sqlx::query(
                "UPDATE chat_sessions SET preferred_provider = NULL, preferred_model = NULL WHERE id = $1",
            )
            .bind(context.session_id)
            .execute(&session_pool)
            .await;

            let ack_id = insert_message(
                &state.db,
                context.session_id,
                context.project_id,
                "assistant",
                "Routing automatico ripristinato. Il sistema sceglierà il modello ottimale per ogni richiesta.",
                json!({ "provider": "system", "model": "auto", "intent": "model_reset" }),
                Some(user_message_id),
            )
            .await?;
            let ack_row = load_message_by_id(&state.db, context.project_id, ack_id).await?;
            let ack_message = to_message_view(&ack_row)?;
            return Ok(Json(json!({
                "sessionId": context.session_id.to_string(),
                "userMessage": user_message,
                "assistantMessage": ack_message,
                "savedAttachments": saved_attachments_json.clone(),
            })));
        }

        if let Some((switched_provider, switched_model)) =
            detect_model_switch(&state.db, content).await
        {
            // Persiste la preferenza nella sessione per i messaggi futuri
            let _ = sqlx::query(
                "UPDATE chat_sessions SET preferred_provider = $1, preferred_model = $2 WHERE id = $3",
            )
            .bind(&switched_provider)
            .bind(&switched_model)
            .bind(context.session_id)
            .execute(&session_pool)
            .await;

            // Genera un messaggio assistant di conferma e salvalo nel DB
            let model_label = switched_model
                .clone()
                .unwrap_or_else(|| switched_provider.clone());
            let ack_content = format!(
                "Modello impostato su **{}**{}. I prossimi messaggi in questa sessione useranno questo provider.",
                switched_provider,
                if switched_model.is_some() {
                    format!(" ({})", model_label)
                } else {
                    String::new()
                }
            );
            let ack_meta = json!({
                "provider": switched_provider,
                "model": model_label,
                "intent": "model_switch",
                "automationMode": "confirm",
            });
            let ack_id = insert_message(
                &state.db,
                context.session_id,
                context.project_id,
                "assistant",
                &ack_content,
                ack_meta,
                Some(user_message_id),
            )
            .await?;
            let ack_row = load_message_by_id(&state.db, context.project_id, ack_id).await?;
            let ack_message = to_message_view(&ack_row)?;

            return Ok(Json(json!({
                "sessionId": context.session_id.to_string(),
                "userMessage": user_message,
                "assistantMessage": ack_message,
                "savedAttachments": saved_attachments_json.clone(),
            })));
        }
    }

    // ── Carica preferenza modello della sessione ────────────────────────────
    // Se l'utente aveva già impostato un provider preferito in questa sessione
    // (tramite un comando precedente "usa mistral"), lo usa come override di default.
    let (session_preferred_provider, session_preferred_model): (Option<String>, Option<String>) =
        if body.provider_override.is_none() {
            sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT preferred_provider, preferred_model FROM chat_sessions WHERE id = $1",
            )
            .bind(context.session_id)
            .fetch_optional(&session_pool)
            .await
            .ok()
            .flatten()
            .map(|(prov, model)| {
                (
                    prov.filter(|s| !s.is_empty()),
                    model.filter(|s| !s.is_empty()),
                )
            })
            .unwrap_or((None, None))
        } else {
            (None, None)
        };
    // Override effettivo: esplicito dal client > preferenza di sessione
    let effective_provider_override = body
        .provider_override
        .clone()
        .or(session_preferred_provider);
    let effective_model_override = body.model_override.clone().or(session_preferred_model);

    let profile_id = body
        .profile_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());

    // Carica contesto profilo (system_prompt, provider/model/automation override)
    // Passa il testo della richiesta per la selezione automatica (profile_id == "auto")
    let (profile_prompt_block, profile_provider, profile_model, profile_automation) =
        fetch_profile_context(&state.db, user_id, &profile_id, &body.content).await;

    // Precedenza modalita' (regola L): workflow d'azione > body > profilo >
    // sessione persistita (mig 0371) > default colonna.
    //
    // Workflow d'azione (agent_type_hint valorizzato: pulsanti error-fix dei
    // pannelli diagnostici via ACTION_AGENT_HINT, service_observer remediation):
    // sono autonomi PER CONTRATTO (l'utente ha chiesto di RISOLVERE, non di
    // proporre). Girano sempre in Automatic, a prescindere dalla modalita' di
    // sessione. Senza questo, l'istruzione DB della modalita' Confirm ("proponi e
    // chiedi conferma prima di procedere") contraddice il prompt d'azione e il fix
    // non viene applicato: l'agente descrive la soluzione invece di eseguirla.
    let automation_mode = if body
        .agent_type_hint
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
    {
        AutomationMode::Automatic
    } else {
        match body
            .automation_mode
            .as_deref()
            .or(profile_automation.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(v) => AutomationMode::try_parse(Some(v)).map_err(|_| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "automation_mode non valido: '{v}'. Valori ammessi: study, confirm, automatic"
                    ),
                )
            })?,
            // chat_sessions e' migrata: la modalita' persistita (mig 0371) va
            // letta dal pool del progetto gia' risolto sopra, non dal meta
            // (dove la riga sessione non esiste e tornava sempre il default).
            None => read_session_automation_mode(&session_pool, context.session_id).await,
        }
    };
    let supervisor_mode = match body
        .supervisor_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(v) => SupervisorMode::try_parse(v).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                format!(
                    "supervisor_mode non valido: '{v}'. Valori ammessi: none, anomaly, interleaved, continuous"
                ),
            )
        })?,
        None => SupervisorMode::None,
    };

    // Fetch user info to build system context
    let github_username: Option<String> =
        sqlx::query_scalar("SELECT github_username FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None)
            .flatten();

    let system_prompt = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.nexus_base",
    )
    .await;
    // Gate a soglia del consiglio di analisi (deterministico, DB-driven): sotto la
    // soglia di complessita' la direttiva <consiglio_analisi> (mig 0549) viene
    // rimossa dal system prompt (task banale -> percorso agentico diretto); sopra
    // soglia resta e il modello convoca le figure. Punto unico: gate_council_directive.
    let system_prompt =
        crate::prompt_templates::gate_council_directive(&state.db, system_prompt, &body.content)
            .await;

    let system_context = {
        let mut ctx = system_prompt;
        if automation_mode != AutomationMode::Study {
            let suffix = crate::prompt_templates::get_template_or_default(
                &state.db,
                &state.template_cache,
                "system.nexus_act_first_suffix",
            )
            .await;
            ctx.push_str(&format!("\n\n{suffix}\n"));
        }
        if let Some(ref gh) = github_username {
            ctx.push_str(&format!(" Account GitHub: @{gh}."));
        }
        // ── Iniezione Knowledge Base (top-K note semanticamente rilevanti) ──
        // Embed del messaggio user → search Qdrant `knowledge_notes` filtrata per progetto
        // → carica title+body delle top hit → prependi come "Contesto progetto" al system prompt.
        // Failsafe: se brain down o KB vuota, il flow normale prosegue (no contesto KB).
        if let Some(kb_block) = build_knowledge_context(&state, context.project_id, content).await {
            ctx.push_str("\n\n");
            ctx.push_str(&kb_block);
        }
        ctx
    };

    // ── Ripresa run interrotto (riprendi / continua / resume) ─────────────
    // Estratto in helper coeso (behavior-preserving): Some(view) => early return
    // con il run risvegliato; None => nessun resume, si prosegue col flusso normale.
    if let Some(resume_view) = try_resume_interrupted_run(
        &state,
        &claims,
        &context,
        &session_pool,
        content,
        automation_mode,
        body.resume,
        user_id,
        user_message_id,
        &user_message,
        &saved_attachments_json,
    )
    .await
    {
        return Ok(Json(resume_view));
    }

    // ── DLP check (Nexus Sicurezza & Privacy) ────────────────────────────────
    // Classifica la sensibilità del contenuto utente prima di inviarlo al brain.
    // Eseguito qui — prima sia di spawn_agent_run sia di run_turn — così copre
    // tutti i percorsi (modalità agente + studio + fallback).
    {
        let tier = crate::dlp::classify_sensitivity(content);
        if tier >= crate::dlp::SensitivityTier::Sensitive {
            // Provider per il check DLP: usa l'override se presente, altrimenti
            // il primo default dalla routing matrix (DB-driven, niente hardcoded).
            let matrix_provider: Option<String> = state
                .orchestrator
                .routing_matrix
                .current()
                .ok()
                .and_then(|m| m.default_models.keys().next().cloned());
            let check_provider = effective_provider_override
                .as_deref()
                .or(matrix_provider.as_deref())
                .unwrap_or("system");
            if let Some(dlp_msg) =
                crate::dlp::check_dlp_policy_db(check_provider, tier, &state.db).await
            {
                if dlp_msg.contains("DLP Block") {
                    // Salva il messaggio di errore come risposta assistant in DB
                    // così l'utente vede il motivo del blocco nell'interfaccia.
                    let err_id = insert_message(
                        &state.db,
                        context.session_id,
                        context.project_id,
                        "assistant",
                        &dlp_msg,
                        json!({
                            "provider": "system",
                            "model": "dlp",
                            "intent": "dlp_block",
                        }),
                        Some(user_message_id),
                    )
                    .await
                    .ok();
                    if let Some(err_msg_id) = err_id {
                        if let Ok(err_row) =
                            load_message_by_id(&state.db, context.project_id, err_msg_id).await
                        {
                            if let Ok(err_msg) = to_message_view(&err_row) {
                                return Ok(Json(json!({
                                    "sessionId": context.session_id.to_string(),
                                    "userMessage": user_message,
                                    "assistantMessage": err_msg,
                                    "dlpBlocked": true,
                                    "savedAttachments": saved_attachments_json.clone(),
                                })));
                            }
                        }
                    }
                    return Err(api_error(StatusCode::FORBIDDEN, dlp_msg));
                } else {
                    tracing::warn!("DLP: {}", dlp_msg);
                }
            }
        }
    }

    // ── Modalita' agente: dispatcha al loop agente invece del singolo turn ──
    if automation_mode != AutomationMode::Study {
        match spawn_agent_run(
            &state,
            SpawnAgentParams {
                user_id,
                session_id: context.session_id,
                project_id: context.project_id,
                user_message_id,
                content: content.to_string(),
                automation_mode,
                supervisor_mode,
                profile_prompt_block,
                system_context: system_context.clone(),
                provider_override: effective_provider_override.clone(),
                model_override: effective_model_override.clone(),
                profile_provider: profile_provider.clone(),
                profile_model: profile_model.clone(),
                attachments: enrich_attachments_with_ids(
                    normalize_attachments(&body.attachments),
                    &saved_attachments_list,
                ),
                nexus_agent_type_hint: body.agent_type_hint.clone(),
            },
        )
        .await
        {
            SpawnOutcome::Started(result) => {
                // Avvia il file watcher anche in modalita' agente asincrona.
                update_user_active_project(&state, user_id, context.project_id).await;
                return Ok(Json(json!({
                    "sessionId": context.session_id.to_string(),
                    "userMessage": user_message,
                    "agentRun": {
                        "runId": result.run_id.to_string(),
                        "status": "running",
                        "provider": result.provider,
                        "model": result.model,
                    },
                    "savedAttachments": saved_attachments_json.clone(),
                })));
            }
            SpawnOutcome::Disambiguation(view) => {
                // Intent ambiguo: la domanda A/B e' gia' stata inserita. Il turno
                // si ferma QUI in attesa della risposta utente: non si deve cadere
                // su run_turn (che eseguirebbe un secondo giro LLM incoerente).
                update_user_active_project(&state, user_id, context.project_id).await;
                return Ok(Json(json!({
                    "sessionId": context.session_id.to_string(),
                    "userMessage": user_message,
                    "assistantMessage": view,
                    "savedAttachments": saved_attachments_json.clone(),
                })));
            }
            SpawnOutcome::NotStarted => {
                // Progetto non caricabile: fallback al singolo turn sotto.
            }
        }
    }

    let run_turn_result = run_turn(
        &state,
        user_id,
        context.session_id,
        context.project_id,
        profile_id,
        content.to_string(),
        user_message_id,
        body.active_files.clone(),
        Some(system_context),
        effective_provider_override,
        effective_model_override,
        automation_mode,
        enrich_attachments_with_ids(
            normalize_attachments(&body.attachments),
            &saved_attachments_list,
        ),
    )
    .await;

    let (assistant_message, orchestrator) = match run_turn_result {
        Ok(result) => result,
        Err(error) => {
            let assistant = fallback_assistant_after_run_turn_error(
                &state,
                context.session_id,
                context.project_id,
                user_message_id,
                &automation_mode,
                &error,
            )
            .await?;
            return Ok(Json(json!({
                "sessionId": context.session_id.to_string(),
                "userMessage": user_message,
                "assistantMessage": assistant,
                "savedAttachments": saved_attachments_json.clone(),
            })));
        }
    };

    let _ = sqlx::query(
        r#"
        UPDATE chat_sessions
        SET
            title = CASE
                WHEN title = 'New Session' OR title = 'Nuova sessione' THEN $2
                ELSE title
            END,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(context.session_id)
    .bind(summarize_title(content))
    .execute(&session_pool)
    .await;

    update_user_active_project(&state, user_id, context.project_id).await;

    Ok(Json(json!({
        "sessionId": context.session_id.to_string(),
        "userMessage": user_message,
        "assistantMessage": assistant_message,
        "run": {
            "id": orchestrator.payload["run_id"].as_str().unwrap_or(""),
            "provider": orchestrator.payload["provider"].as_str().unwrap_or(""),
            "model": orchestrator.payload["model"].as_str().unwrap_or(""),
            "intent": orchestrator.payload["intent"].as_str().unwrap_or("chat"),
        },
        "savedAttachments": saved_attachments_json,
    })))
}

/// Ripresa run interrotto (riprendi / continua / resume), estratta da
/// send_chat_message (behavior-preserving). Some(view) => il resume e' partito
/// e il chiamante fa early-return; None => nessun resume, flusso normale.
#[allow(clippy::too_many_arguments)]
async fn try_resume_interrupted_run(
    state: &AppState,
    claims: &Claims,
    context: &crate::chat_sessions::SessionContext,
    session_pool: &PgPool,
    content: &str,
    automation_mode: AutomationMode,
    force_resume: bool,
    user_id: Uuid,
    user_message_id: Uuid,
    user_message: &ChatMessageView,
    saved_attachments_json: &Value,
) -> Option<Value> {
    if automation_mode == AutomationMode::Study {
        return None;
    }

    // `force_resume` (regola N): segnale STRUTTURATO dal pulsante "Riattiva" del
    // banner chat-sospesa, indipendente dal testo. La stringa magica ("riprendi"/
    // "continua") resta come scorciatoia digitabile ma non e' piu' l'unico canale.
    let is_resume_request = force_resume || {
        let lower = content.trim().to_lowercase();
        lower == "riprendi"
            || lower == "continua"
            || lower == "resume"
            || lower == "riprendi dall'interruzione"
            || lower.starts_with("riprendi ")
            || lower.starts_with("continua da")
    };
    if !is_resume_request {
        return None;
    }

    // Cerca l'ultimo run interrupted di questa sessione con history salvata
    let prev_run = sqlx::query(
        r#"SELECT id, provider, model, messages_json, iteration_count, supervisor_mode
           FROM agent_runs
           WHERE session_id = $1
             AND status = 'interrupted'
             AND messages_json IS NOT NULL
             AND messages_json != ''
           ORDER BY created_at DESC
           LIMIT 1"#,
    )
    .bind(context.session_id)
    .fetch_optional(session_pool)
    .await
    .ok()
    .flatten()?;

    let prev_run_id: Uuid = prev_run.get("id");
    let prev_provider: String = prev_run.get("provider");
    let prev_model: String = prev_run.get("model");
    let prev_messages_json: String = prev_run.get("messages_json");
    let prev_iterations: i32 = prev_run.get("iteration_count");

    tracing::info!(
        "Resuming interrupted run {} (iter={}, supervisor={}) for session {}",
        prev_run_id,
        prev_iterations,
        prev_run
            .try_get::<String, _>("supervisor_mode")
            .unwrap_or_else(|_| "none".into()),
        context.session_id
    );

    // Crea nuovo run collegato al precedente
    let new_run_id = Uuid::new_v4();
    let (tx, _rx) = broadcast::channel::<AgentStepEvent>(256);
    state.agent_channels.insert(new_run_id, tx.clone());

    let prev_supervisor_str: String = prev_run
        .try_get("supervisor_mode")
        .unwrap_or_else(|_| "none".to_string());
    let prev_supervisor = prev_supervisor_str.parse::<SupervisorMode>().unwrap();

    // Last-wins atomico (punto unico, regola L): cancella TUTTI i run
    // attivi della sessione (incluso il precedente) PRIMA di inserire
    // il nuovo. Elimina la race "INSERT-poi-UPDATE" che lasciava due
    // run attivi per una finestra, e ferma cooperativamente il vecchio.
    let _ =
        crate::chat_messages::agent_run::supersede_active_runs(state, context.session_id, "resume")
            .await;

    let _ = sqlx::query(
        r#"INSERT INTO agent_runs
           (id, session_id, project_id, user_id, run_message_id, status,
            automation_mode, provider, model, supervisor_mode, iteration_count, parent_run_id, created_at)
           VALUES ($1,$2,$3,$4,$5,'running',$6,$7,$8,$9,0,$10,NOW())"#,
    )
    .bind(new_run_id)
    .bind(context.session_id)
    .bind(context.project_id)
    .bind(user_id)
    .bind(user_message_id)
    .bind(automation_mode.as_str())
    .bind(&prev_provider)
    .bind(&prev_model)
    .bind(prev_supervisor.as_str())
    .bind(prev_run_id)
    .execute(session_pool)
    .await;

    // Carica contesto progetto per il nuovo run
    let Ok(proj) = load_project_context(&state.db, context.project_id, user_id).await else {
        // Comportamento storico: senza contesto progetto il resume non parte
        // e il chiamante prosegue col flusso normale.
        return None;
    };

    let state_for_task = state.clone();
    let db_clone2 = state.db.clone();
    let channels2 = state.agent_channels.clone();
    let proj_channels2 = state.project_channels.clone();
    let neural2 = state.orchestrator.neural.clone();
    let term2 = state.terminal_consumers.clone();
    let session_id_r = context.session_id;
    let project_id_r = context.project_id;
    let msg_id_r = user_message_id;
    let provider_r = prev_provider.clone();
    let model_r = prev_model.clone();
    let automation_r = automation_mode;
    let supervisor_r = prev_supervisor;
    let template_cache_r = state.template_cache.clone();
    let user_role_r = claims.role.clone();

    let _ = (
        &neural2,
        &term2,
        &automation_r,
        &supervisor_r,
        &user_role_r,
        &proj,
        &prev_messages_json,
    );
    tokio::spawn(async move {
        // Separazione DB: chat_messages/agent_runs sono migrate -> pool del
        // progetto risolto UNA volta e riusato per history, UPDATE, INSERT,
        // finalize e worklog. db_clone2 resta il meta SOLO per template,
        // ledger e catalogo tool (domini di piattaforma). Senza il DB del
        // progetto il resume non puo' ne' leggere la history ne' finalizzare
        // il run: si abortisce il task con ERROR (regola M), mai sul meta.
        let proj_pool = match crate::project_db_routes::project_data_pool_from(
            &db_clone2,
            project_id_r,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(
                    run_id = %new_run_id,
                    project_id = %project_id_r,
                    error = %e,
                    "resume: DB progetto non disponibile, task di resume abortito"
                );
                channels2.remove(&new_run_id);
                return;
            }
        };
        let resume_tpl = crate::prompt_templates::get_template_or_default(
            &db_clone2,
            &template_cache_r,
            "automation.run_resume_instruction",
        )
        .await;
        let resume_prompt = resume_tpl.replace("{{prev_iterations}}", &prev_iterations.to_string());

        // History dal DB del progetto: sul meta chat_messages e' vuota a flag
        // ON e il run ripreso ripartiva senza contesto conversazionale.
        let resume_history = build_recent_conversation_history(&proj_pool, session_id_r, 8).await;

        let tools_for_resume = crate::agent_turn_setup::build_tools_json_for_agent(
            &db_clone2,
            user_id,
            project_id_r,
            &automation_r,
            &provider_r,
            &model_r,
        )
        .await;

        // Il resume gira sul motore NATIVO, come ogni altro run.
        //
        // Qui si chiamava `run_via_brain` SENZA alcun controllo del motore: era
        // l'unico call site non gated da `select_engine`, quindi dal giorno della
        // rimozione del brain ogni "riprendi" dell'utente e' finito su un
        // servizio inesistente. Non era una configurazione esotica: e' il flusso
        // normale di chi scrive "continua" in chat.
        let native_input = crate::native_engine::NativeRunInput {
            run_id: new_run_id,
            session_id: session_id_r,
            provider: provider_r.clone(),
            model: model_r.clone(),
            system_text: String::new(),
            initial_msg: resume_prompt,
            conversation_history: resume_history,
            tools_json: tools_for_resume,
            // Resume di un run interrotto: nessuna disambiguazione da risolvere.
            intent_hint: None,
            // Il classifier non ha girato per questo turno: i campi restano
            // neutri e `classifier_resolved=false` lo DICHIARA, cosi' il
            // RouterNode decide invece di fidarsi di valori inventati.
            requires_tools: None,
            agentic_score: None,
            authorizes_changes: None,
            classifier_resolved: false,
            action_oriented_min_score: crate::intent_classifier::DEFAULT_ACTION_ORIENTED_MIN_SCORE,
            automation_mode: automation_r.as_str().to_string(),
            supervisor_mode: crate::native_engine::graph_supervisor_mode(supervisor_r),
            step_tx: tx,
            parent_run_id: None,
            subagent_depth: None,
            sizing_complexity: None,
            sizing_scope_system_wide: false,
            classifier_intent: None,
            run_time_budget_s: None,
            working_root: None,
            // I panel a monte hanno gia' deliberato sul run originale: il resume
            // riprende il lavoro, non riapre la deliberazione.
            pre_run_advisory_synthesis: None,
            pre_run_advisory_source: None,
            advisory_gate: None,
        };
        let mut result = match crate::chat_messages::agent_run::run_via_native(
            &state_for_task,
            &native_input,
        )
        .await
        {
            Ok(outcome) => {
                match crate::project_db_routes::project_data_pool_from(&db_clone2, project_id_r)
                    .await
                {
                    Ok(steps_pool) => {
                        crate::chat_messages::agent_run::native_outcome_to_run_result(
                            &steps_pool,
                            new_run_id,
                            outcome,
                        )
                        .await
                    }
                    Err(e) => {
                        tracing::error!(
                            run_id = %new_run_id,
                            error = %e,
                            "resume: DB progetto non disponibile al finalize"
                        );
                        crate::chat_messages::agent_run::native_engine_failure_result(
                            new_run_id,
                            &provider_r,
                            &model_r,
                            format!("DB del progetto non disponibile al finalize: {e}"),
                        )
                    }
                }
            }
            Err(e) => {
                tracing::error!(run_id = %new_run_id, error = %e, "resume: motore nativo fallito");
                crate::chat_messages::agent_run::native_engine_failure_result(
                    new_run_id,
                    &provider_r,
                    &model_r,
                    format!("Il resume non e' riuscito: {e}"),
                )
            }
        };
        channels2.remove(&new_run_id);

        // Riconciliazione costo/token dal ledger (punto unico,
        // regola L): se il path brain non propaga il costo ma il
        // gateway ha gia' scritto il ledger per il run, il metadata
        // del messaggio assistant (e quindi la UI) mostra il costo
        // reale invece di $0.00.
        let ledger_totals =
            crate::chat_messages::agent_run::fetch_ledger_totals(&db_clone2, new_run_id).await;
        let cost_reconciled = crate::chat_messages::agent_run::reconcile_run_cost_from_ledger(
            &mut result,
            &ledger_totals,
        );
        // finalize_agent_run NON scrive token/costo: se il ledger li ha, allinea
        // qui (idempotente: la WHERE tocca solo i run rimasti a 0).
        //
        // La condizione e' il SEGNALE di riconciliazione, non `total_cost > 0`
        // (regola M, gemello di `agent_run.rs`): un run contabilizzato a costo 0
        // perche' il prezzo del modello e' ignoto ha comunque token reali da
        // allineare, e la vecchia soglia lo saltava.
        if cost_reconciled {
            // agent_runs e' migrata: instrada sul pool del progetto
            // (risolto in-task da db_clone2 + project_id_r, come la
            // INSERT chat_messages piu' sotto). ai_usage_ledger (sopra)
            // resta su meta (dominio costi non migrato).
            let _ = sqlx::query(
                "UPDATE agent_runs SET total_cost = $2, total_tokens = $3 \
                 WHERE id = $1 AND total_cost = 0",
            )
            .bind(new_run_id)
            .bind(result.total_cost)
            .bind(result.total_tokens as i32)
            .execute(&proj_pool)
            .await;
        }

        // Esito CERTO anche sul resume (regola L + ADR 0025): stesso
        // punto unico dello spawn principale. compose_turn_answer
        // garantisce un messaggio (risposta reale + recap, oppure
        // recap/placeholder se hollow); canonical_run_status declassa
        // l'hollow-senza-lavoro a failed_diagnosed. Prima il resume
        // inseriva nulla per i run vuoti e finalizzava lo status grezzo.
        let resume_answer = crate::chat_messages::agent_run::compose_turn_answer(&result);
        // Recap narrativo opzionale (mig 0415): stesso punto unico
        // del finalize spawn. Gate off di default -> no-op.
        let resume_answer =
            crate::chat_messages::agent_run::narrative_or(&state_for_task, &result, resume_answer)
                .await;
        let resume_status = crate::chat_messages::agent_run::canonical_run_status(&result);
        if let Some(answer) = resume_answer.clone() {
            // Recap RICCO in coda anche sul resume (FIX D3, regola L):
            // stesso punto unico append_outcome_summary dello spawn,
            // cosi' il content persistito coincide col recap live e
            // non diverge dopo un refresh.
            let answer =
                crate::chat_messages::agent_run::append_outcome_summary(answer, &result.steps);
            let mut meta = serde_json::json!({
                "provider": &result.provider,
                "model": &result.model,
                "agentRunId": new_run_id.to_string(),
                "iterationCount": result.iteration_count,
                "automationMode": "automatic",
                "resumed": true,
                "hollowCompletion": result.hollow_completion,
                "promptTokens": result.prompt_tokens,
                "completionTokens": result.completion_tokens,
                "totalTokens": result.total_tokens,
                "totalCost": result.total_cost,
                "currency": "USD",
            });
            // FIX D4: persisti il reasoning anche sul resume cosi'
            // il blocco "Ragionamento" sopravvive al refresh.
            if let Some(reasoning) = result
                .reasoning
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert("reasoning".to_string(), serde_json::json!(reasoning));
                }
            }
            let _ = sqlx::query(
                r#"INSERT INTO chat_messages
                   (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                   VALUES (gen_random_uuid(),$1,$2,'assistant',$3,$4,$5,NOW())"#,
            )
            .bind(session_id_r)
            .bind(project_id_r)
            .bind(&answer)
            .bind(meta)
            .bind(msg_id_r)
            .execute(&proj_pool)
            .await;
        }

        let _run_completed = resume_status.is_success();
        let resume_status_str = resume_status.as_str();
        // Finalize sul DB del progetto: sul meta l'UPDATE non trovava il run e
        // il run resumed restava 'running' bloccando la sessione (gate 409).
        crate::agent_types::finalize_agent_run(
            &proj_pool,
            new_run_id,
            resume_status,
            resume_answer.as_deref().or(result.final_answer.as_deref()),
            result.iteration_count,
        )
        .await;

        // Worklog di sessione (mig 0411): stesso hook del
        // percorso spawn principale — anche il resume di
        // conferma alimenta la storia di lavoro. Best-effort.
        // Worklog nel DB del progetto (separazione DB), pool per-progetto.
        if let Err(e) = crate::session_worklog::ingest_steps_for_run(
            &db_clone2,
            &proj_pool,
            session_id_r,
            Some(project_id_r),
            new_run_id,
            resume_status_str,
            &result.steps,
        )
        .await
        {
            tracing::warn!(error = %e, "session_worklog: ingest al resume fallito");
        }

        // ADR 0017 v2 TODO 7: il worker
        // `wiki::run_summary_worker` (avviato in main.rs,
        // intervallo 60s default) ingesta i run terminali in
        // `wiki_docs` (scope=project, kind='run_summary').
        // Idempotenza via `agent_runs.kb_ingested` (mig 0304).
        let _ = (
            &db_clone2,
            &neural2,
            &proj_channels2,
            new_run_id,
            _run_completed,
        );
    });

    Some(json!({
        "sessionId": context.session_id.to_string(),
        "userMessage": user_message,
        "agentRun": {
            "runId": new_run_id.to_string(),
            "status": "running",
            "provider": prev_provider,
            "model": prev_model,
            "resumed": true,
        },
        "savedAttachments": saved_attachments_json.clone(),
    }))
}

pub async fn resend_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
    Json(body): Json<SendChatMessageRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    // Separazione DB: chat_messages/chat_sessions/chat_message_attachments migrate
    // nel DB del progetto. Risolvo una volta il pool del progetto a partire dal
    // message_id (Path param) e lo riuso per tutte le SELECT su queste tabelle. Il
    // JOIN chat_messages+chat_sessions e' su un solo pool perche' entrambe vivono
    // nello stesso <slug>_nexus. DB non disponibile -> 503 strutturato.
    let msg_pool =
        crate::project_db_routes::project_data_pool_by_message_from(&state.db, message_id).await?;

    let row = sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.content,
            m.request_message_id,
            m.created_at,
            s.user_id
        FROM chat_messages m
        JOIN chat_sessions s ON s.id = m.session_id
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_optional(&msg_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"));
    };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    if owner != Some(user_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Messaggio non accessibile",
        ));
    }

    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let role: String = row
        .try_get("role")
        .unwrap_or_else(|_| "assistant".to_string());
    let source_user_message_id: Uuid = if role == "user" {
        message_id
    } else if let Some(request_message_id) = row
        .try_get::<Option<Uuid>, _>("request_message_id")
        .unwrap_or(None)
    {
        request_message_id
    } else {
        let created_at: DateTime<Utc> = row
            .try_get("created_at")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id
            FROM chat_messages
            WHERE session_id = $1
              AND role = 'user'
              AND created_at <= $2
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(session_id)
        .bind(created_at)
        .fetch_optional(&msg_pool)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Impossibile determinare il messaggio utente da reinviare",
            )
        })?
    };

    let source_prompt = sqlx::query_scalar::<_, String>(
        r#"
        SELECT content
        FROM chat_messages
        WHERE id = $1
          AND role = 'user'
        "#,
    )
    .bind(source_user_message_id)
    .fetch_optional(&msg_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Messaggio utente originale non trovato",
        )
    })?;
    let source_metadata = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT metadata
        FROM chat_messages
        WHERE id = $1
        "#,
    )
    .bind(source_user_message_id)
    .fetch_optional(&msg_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_else(|| json!({}));

    let profile_id = body
        .profile_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());
    let provider_override = body.provider_override.clone().or_else(|| {
        source_metadata
            .get("providerOverride")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    let model_override = body.model_override.clone().or_else(|| {
        source_metadata
            .get("modelOverride")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    let automation_mode = if body.automation_mode.is_some() {
        parse_automation_mode(body.automation_mode.as_deref())
    } else {
        // RC-4 (regola N): prima il mode del messaggio originale (replay del mode di
        // allora); se assente/vuoto (messaggi vecchi col metadata 'confirm' spurio),
        // FALLBACK al mode PERSISTITO sulla sessione (pool progetto), mai un default
        // hardcoded. Cosi' un resend non degrada silenziosamente a Confirm.
        match source_metadata
            .get("automationMode")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(v) => parse_automation_mode(Some(v)),
            None => read_session_automation_mode(&msg_pool, session_id).await,
        }
    };
    let attachments_raw = if body.attachments.is_empty() {
        serde_json::from_value::<Vec<ChatAttachmentRequest>>(
            source_metadata
                .get("attachments")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map(|value| normalize_attachments(&value))
        .unwrap_or_default()
    } else {
        normalize_attachments(&body.attachments)
    };
    // Resend: gli allegati sono gia' persistiti nel turno originale, recupero
    // gli UUID dal DB per il source_user_message_id. Senza questo il blocco
    // <allegati> nel prompt iniziale del retry non avrebbe gli ID e il modello
    // re-incappa nel bug del fallback al filename (vedi
    // enrich_attachments_with_ids_from_db).
    let attachments =
        enrich_attachments_with_ids_from_db(&msg_pool, attachments_raw, source_user_message_id)
            .await;
    let attachments_metadata = if body.attachments.is_empty() {
        source_metadata
            .get("attachments")
            .cloned()
            .unwrap_or_else(|| json!([]))
    } else {
        json!(body.attachments.clone())
    };

    let resent_user_message_id = insert_message(
        &state.db,
        session_id,
        project_id,
        "user",
        &source_prompt,
        json!({
            "providerOverride": provider_override.clone(),
            "modelOverride": model_override.clone(),
            "automationMode": automation_mode.as_str(),
            "attachments": attachments_metadata,
            "resendOf": source_user_message_id.to_string(),
        }),
        None,
    )
    .await?;
    let resent_user_row = load_message_by_id(&state.db, project_id, resent_user_message_id).await?;
    let resent_user_message = to_message_view(&resent_user_row)?;

    // ── Agent mode per resend (usa la stessa funzione condivisa di send) ──
    if automation_mode != AutomationMode::Study {
        let (profile_prompt_block, _, _, _) =
            fetch_profile_context(&state.db, user_id, &profile_id, &source_prompt).await;
        let github_username: Option<String> =
            sqlx::query_scalar("SELECT github_username FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&state.db)
                .await
                .unwrap_or(None)
                .flatten();
        let system_context_str = {
            let mut ctx = String::from(
                "Sei Nexus, agente operativo di sviluppo. Regole:\n\
                 Output: testo pulito, markdown standard (no emoji, no caratteri grafici).\n\
                 Tool iniziali: read_file, list_files, search_in_files, write_file, edit_file, run_command.\n\
                 Tool aggiuntivi: usa request_tools(categories) per sbloccare categorie extra:\n\
                 - \"git\": git_status, git_stage, git_commit, git_push, git_pull\n\
                 - \"service\": run_service, read_service_output, stop_service\n\
                 - \"files_advanced\": delete_file, rename_file\n\
                 - \"profile\": create_profile, update_profile\n\
                 - \"subtask\": dispatch_subtask\n\
                 - \"mcp\": tool da server MCP esterni\n\
                 Autonomia: NON chiedere mai struttura, tecnologia, OS, comandi — ricava tutto dal contesto progetto o con list_files/read_file.\n\
                 PERO' SE ti mancano informazioni che NON puoi ricavare autonomamente (connection string, API keys, credenziali, \
                 configurazioni specifiche dell'ambiente, password, URL di servizi esterni), DEVI chiedere all'utente. \
                 Non tentare di indovinare valori sensibili. Interrompi il flusso, spiega cosa ti serve e perche', e attendi la risposta.\n\
                 File grandi — REGOLA CRITICA PER PERFORMANCE:\n\
                 read_file restituisce solo le prime 300 righe. Se il file e' piu' grande, usa questo flusso:\n\
                 1. read_file(path) — ottieni le prime 300 righe + totale righe\n\
                 2. read_file_lines(path, start_line, end_line) — leggi un range specifico (max 400 righe per chiamata)\n\
                 3. Se non sai dove si trova la sezione: usa search_in_files o search_codebase_semantic, poi read_file_lines\n\
                 NON caricare file interi grandi. Usa sempre lettura chirurgica per sezioni specifiche.\n\
                 Avvio servizi — REGOLE TASSATIVE:\n\
                 1) Per avviare servizi (server, watcher, processi long-running), usa run_service con label descrittiva.\n\
                 2) Dopo OGNI run_service, LEGGI l'output restituito. Se serve piu' output, usa read_service_output col process_id.\n\
             ANTI-LOOP: non chiamare read_service_output piu' di 3 volte consecutive sullo stesso process_id. Se dopo 3 letture il servizio non e' pronto, smetti di aspettare e riferisci all'utente lo stato attuale. Non eseguire run_command in loop per monitorare uno stesso processo.\n\
                 3) Se l'output contiene errori (exit code != 0, Error, Exception, failed), CORREGGI e RILANCIA (stop_service + run_service).\n\
                 4) Dopo che i servizi sono avviati, VERIFICA con run_command(\"ss -tlnp | grep PORTA\") che le porte siano in ascolto.\n\
                 5) Nella risposta finale, fornisci SEMPRE i link URL (es. http://localhost:5000, http://localhost:5173) dove l'utente puo' aprire i servizi.\n\
                 Errori comuni e correzioni:\n\
                 - Porta occupata: run_command(\"lsof -t -i:PORTA | xargs kill -9\") poi rilancia\n\
                 - .NET TargetFramework errato: controlla con run_command(\"dotnet --list-sdks\"), aggiorna .csproj, rilancia\n\
                 - Build fallita: leggi output, correggi con edit_file, rilancia\n\
                 - npm module not found: run_command(\"npm install\") poi rilancia\n\
                 - SEMPRE rilancia dopo una correzione. Mai fermarsi dopo un fix senza verificare.\n\
                 Persistenza: se un'operazione fallisce, leggi l'errore, analizzalo e riprova. Non arrenderti al primo errore.\n\
                 Git: usa credenziali utente autenticato. Per cloni parti da $NEXUS_TERMINAL_ROOT.\n\
                 Profili: quando noti stack tecnico ricorrente, crea/aggiorna profilo con create_profile/update_profile.",
            );
            if automation_mode != AutomationMode::Study {
                let suffix = crate::prompt_templates::get_template_or_default(
                    &state.db,
                    &state.template_cache,
                    "system.nexus_act_first_suffix",
                )
                .await;
                ctx.push_str(&format!("\n\n{suffix}\n"));
            }
            if let Some(ref gh) = github_username {
                ctx.push_str(&format!(" Account GitHub: @{gh}."));
            }
            ctx
        };

        match spawn_agent_run(
            &state,
            SpawnAgentParams {
                user_id,
                session_id,
                project_id,
                user_message_id: resent_user_message_id,
                content: source_prompt.clone(),
                automation_mode,
                supervisor_mode: SupervisorMode::default(),
                profile_prompt_block,
                system_context: system_context_str,
                provider_override: provider_override.clone(),
                model_override: model_override.clone(),
                profile_provider: None,
                profile_model: None,
                attachments: attachments.clone(),
                nexus_agent_type_hint: None, // resend non usa hint
            },
        )
        .await
        {
            SpawnOutcome::Started(result) => {
                update_user_active_project(&state, user_id, project_id).await;
                return Ok(Json(json!({
                    "sessionId": session_id.to_string(),
                    "userMessage": resent_user_message,
                    "agentRun": {
                        "runId": result.run_id.to_string(),
                        "status": "running",
                        "provider": result.provider,
                        "model": result.model,
                    }
                })));
            }
            SpawnOutcome::Disambiguation(view) => {
                // Intent ambiguo: domanda di chiarimento gia' inserita, il turno
                // si ferma in attesa della risposta utente (no fallback run_turn).
                update_user_active_project(&state, user_id, project_id).await;
                return Ok(Json(json!({
                    "sessionId": session_id.to_string(),
                    "userMessage": resent_user_message,
                    "assistantMessage": view,
                })));
            }
            SpawnOutcome::NotStarted => {
                // Fallback al singolo turno sotto.
            }
        }
    }

    // Fallback: orchestrator singolo turno (Study mode o progetto non trovato)
    let run_turn_result = run_turn(
        &state,
        user_id,
        session_id,
        project_id,
        profile_id,
        source_prompt.clone(),
        resent_user_message_id,
        body.active_files.clone(),
        None,
        provider_override,
        model_override,
        automation_mode,
        attachments,
    )
    .await;

    // Stessa gestione di send_chat_message (regola L): se la cascata provider e'
    // esaurita run_turn fallisce; invece di propagare l'errore grezzo (che lato
    // chat diventava 500 / socket hang up sul resend) ritorniamo un messaggio
    // assistant gestito con HTTP 200.
    let (assistant_message, orchestrator) = match run_turn_result {
        Ok(result) => result,
        Err(error) => {
            let assistant = fallback_assistant_after_run_turn_error(
                &state,
                session_id,
                project_id,
                resent_user_message_id,
                &automation_mode,
                &error,
            )
            .await?;
            update_user_active_project(&state, user_id, project_id).await;
            return Ok(Json(json!({
                "sessionId": session_id.to_string(),
                "userMessage": resent_user_message,
                "assistantMessage": assistant,
            })));
        }
    };

    update_user_active_project(&state, user_id, project_id).await;

    Ok(Json(json!({
        "sessionId": session_id.to_string(),
        "userMessage": resent_user_message,
        "assistantMessage": assistant_message,
        "run": {
            "id": orchestrator.payload["run_id"].as_str().unwrap_or(""),
            "provider": orchestrator.payload["provider"].as_str().unwrap_or(""),
            "model": orchestrator.payload["model"].as_str().unwrap_or(""),
        }
    })))
}
/// Punto unico (regola L) per la risposta di fallback quando `run_turn` fallisce
/// (es. cascata provider esaurita: tutti in cooldown/billing KO). Invece di
/// propagare l'errore grezzo con `?` — che lato frontend si manifesta come
/// 500 / "socket hang up" — inserisce un messaggio assistant "Operazione non
/// completata: ..." e ne ritorna la view serializzata. Usato sia da
/// `send_chat_message` sia da `resend_chat_message`, che prima divergevano: il
/// primo gestiva l'errore, il secondo lo lasciava propagare (bug del 500 chat).
async fn fallback_assistant_after_run_turn_error(
    state: &AppState,
    session_id: Uuid,
    project_id: Uuid,
    user_message_id: Uuid,
    automation_mode: &AutomationMode,
    error: &(StatusCode, Json<Value>),
) -> Result<Value, (StatusCode, Json<Value>)> {
    let err_text = error.1["error"].as_str().unwrap_or("generation_error");
    // La frase gia' resa a monte, quando i fatti erano vivi (`api_error_rendered`
    // su run_turn). Quando c'e' si USA: ri-renderla da `err_text` darebbe il
    // messaggio generico del dominio Gateway, perche' da qui provider, modello e
    // status non sono piu' leggibili.
    let user_message = error.1["user_message"].as_str().map(str::to_string);
    let user_code = error.1["user_code"].as_str().unwrap_or("");
    let fallback_metadata = json!({
        "provider": "none",
        "model": "none",
        "intent": "chat",
        "runId": "",
        "error": err_text,
        // Identificatore canonico della classe: e' il campo su cui la UI puo'
        // decidere un'azione (riprova, cambia modello, ricarica credito) senza
        // guardare il testo (regola M). CamelCase come i vicini di questo
        // metadata (promptTokens, totalCost), non snake_case come sul wire del
        // gateway: la convenzione locale vince sulla coerenza cross-confine.
        "userCode": user_code,
        "promptTokens": 0,
        "completionTokens": 0,
        "totalTokens": 0,
        "totalCost": 0.0,
        "currency": "EUR",
        "automationMode": automation_mode.as_str(),
    });
    let assistant_id = insert_message(
        &state.db,
        session_id,
        project_id,
        "assistant",
        // Il testo tecnico NON entra nel corpo del messaggio: e' gia' in
        // `fallback_metadata["error"]`, da cui il pannello diagnostico lo legge.
        // Qui viveva `humanize_ai_error`, che decideva la frase cercando "429" o
        // "timeout" DENTRO il testo (regola M) e, quando non li trovava, ci
        // incollava la prima riga troncata a 220 caratteri — cioe' il blob
        // mozzato che si leggeva in chat.
        //
        // I fatti opachi restano il RIPIEGO onesto: valgono per gli errori che
        // non attraversano il gateway (validazione, DB, permessi), dove non c'e'
        // nessuna resa da trasportare.
        &format!(
            "Operazione non completata: {}",
            user_message.unwrap_or_else(|| nexus_types::error_presentation::render_user_error(
                &nexus_types::error_presentation::ErrorFacts::opaque(
                    nexus_types::error_presentation::ErrorDomain::Gateway,
                    err_text,
                ),
            )
            .message)
        ),
        fallback_metadata,
        Some(user_message_id),
    )
    .await?;
    let row = load_message_by_id(&state.db, project_id, assistant_id).await?;
    let assistant = to_message_view(&row)?;
    serde_json::to_value(assistant)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub async fn delete_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    // Separazione DB: endpoint keyed solo dal message_id. chat_messages/chat_sessions
    // vivono nel DB del progetto -> pool via directory di routing (fallback ricerca).
    let mpool =
        crate::project_db_routes::project_data_pool_by_message_from(&state.db, message_id).await?;

    let row = sqlx::query(
        r#"
        UPDATE chat_messages m
        SET deleted_at = NOW(),
            deleted_by_user_id = $2,
            updated_at = NOW()
        FROM chat_sessions s
        WHERE m.id = $1
          AND m.session_id = s.id
          AND s.user_id = $2
        RETURNING m.id, m.session_id, s.project_id
        "#,
    )
    .bind(message_id)
    .bind(user_id)
    .fetch_optional(&mpool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Messaggio non trovato o non autorizzato",
        ));
    };

    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    update_user_active_project(&state, user_id, project_id).await;

    Ok(Json(json!({
        "ok": true,
        "messageId": message_id.to_string()
    })))
}
pub async fn feedback_error(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
    Json(body): Json<FeedbackErrorRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;
    let comment = body.comment.trim();
    if comment.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il commento di errore e' obbligatorio",
        ));
    }

    // Separazione DB: endpoint keyed solo dal message_id. Risolvo il pool del
    // progetto dalla directory di routing (fallback ricerca + auto-registrazione);
    // chat_messages/chat_sessions/ai_response_feedback/prompt_corrections vivono
    // li'. ensure_project_access resta sul meta (globale).
    let mpool =
        crate::project_db_routes::project_data_pool_by_message_from(&state.db, message_id).await?;

    let row = sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.content,
            m.metadata,
            s.user_id
        FROM chat_messages m
        JOIN chat_sessions s ON s.id = m.session_id
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_optional(&mpool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"));
    };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    if owner != Some(user_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Messaggio non accessibile",
        ));
    }

    let role: String = row.try_get("role").unwrap_or_default();
    if role != "assistant" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il feedback errore e' consentito solo sui messaggi AI",
        ));
    }

    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let metadata: Value = row.try_get("metadata").unwrap_or_else(|_| json!({}));
    let ai_response_content: String = row.try_get("content").unwrap_or_default();
    let intent = metadata
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .to_lowercase();
    let provider = metadata
        .get("provider")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let model = metadata
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let raw_run_id = metadata
        .get("runId")
        .or_else(|| metadata.get("agentRunId"))
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    // Verifica esistenza in orchestrator_runs (FK target).
    // I run agent moderni vivono in `agent_runs`, ma la FK del feedback punta
    // a `orchestrator_runs`. Se l'ID non esiste li', settiamo NULL invece di
    // far fallire l'insert (la colonna ammette NULL).
    let run_id: Option<Uuid> = match raw_run_id {
        Some(id) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM orchestrator_runs WHERE id = $1)")
                    .bind(id)
                    .fetch_one(&mpool)
                    .await
                    .unwrap_or(false);
            if exists {
                Some(id)
            } else {
                None
            }
        }
        None => None,
    };

    // Recupera il messaggio utente precedente nella stessa sessione:
    // è la domanda che ha generato questa risposta AI — usata per costruire
    // un embedding semanticamente ricco che matchi domande simili future.
    let preceding_user_message: Option<String> = sqlx::query_scalar(
        r#"
        SELECT content FROM chat_messages
        WHERE session_id = $1
          AND role = 'user'
          AND created_at < (SELECT created_at FROM chat_messages WHERE id = $2)
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(session_id)
    .bind(message_id)
    .fetch_optional(&mpool)
    .await
    .unwrap_or(None);

    // ── Testo per l'embedding ──────────────────────────────────────────────
    // Concatena domanda utente + commento di correzione.
    // Così quando arriva una domanda semanticamente simile in futuro,
    // il vettore viene trovato con alta similarità.
    let embed_input = match &preceding_user_message {
        Some(q) if !q.is_empty() => format!(
            "{}\n\nCorrezione: {}",
            q.chars().take(800).collect::<String>(),
            comment
        ),
        _ => comment.to_string(),
    };

    // ── correction_text: testo che viene iniettato nel system prompt ───────
    // Deve essere una istruzione chiara e azionabile per l'AI.
    let correction_text = match &preceding_user_message {
        Some(q) if !q.is_empty() => format!(
            "[{}] Quando viene chiesto: «{}» — {}",
            intent,
            q.chars().take(200).collect::<String>(),
            comment
        ),
        _ => format!("[{}] {}", intent, comment),
    };

    // Preview della risposta AI sbagliata (per audit/debug, max 500 chars)
    let ai_response_preview: String = ai_response_content.chars().take(500).collect();

    // ai_response_feedback e prompt_corrections vivono nel DB del progetto (mpool);
    // registro feedback_id/correction_id in directory dopo l'insert cosi' gli
    // endpoint admin by-id (review/delete) risolvono il pool.
    let feedback_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO ai_response_feedback (
            id, project_id, session_id, message_id, orchestrator_run_id, user_id,
            feedback_type, intent, provider, model, error_comment, status, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'error', $7, $8, $9, $10, 'open', NOW(), NOW())
        "#,
    )
    .bind(feedback_id)
    .bind(project_id)
    .bind(session_id)
    .bind(message_id)
    .bind(run_id)
    .bind(user_id)
    .bind(&intent)
    .bind(&provider)
    .bind(&model)
    .bind(comment)
    .execute(&mpool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::project_db_routes::register_entity_routing(
        &state.db,
        "feedback",
        feedback_id,
        project_id,
    )
    .await;

    let correction_id = Uuid::new_v4();
    let point_id = correction_id.to_string();
    let normalized = normalize_text(&correction_text);
    let normalized_hash = hash_hint(project_id, &intent, &normalized);

    sqlx::query(
        r#"
        INSERT INTO prompt_corrections (
            id, project_id, feedback_id, session_id, message_id, orchestrator_run_id,
            intent, provider, model, correction_text, normalized_hint_hash, qdrant_point_id,
            active, status, metadata, created_at, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
            TRUE, 'open', $13, NOW(), NOW()
        )
        "#,
    )
    .bind(correction_id)
    .bind(project_id)
    .bind(feedback_id)
    .bind(session_id)
    .bind(message_id)
    .bind(run_id)
    .bind(&intent)
    .bind(&provider)
    .bind(&model)
    .bind(&correction_text)
    .bind(&normalized_hash)
    .bind(&point_id)
    .bind(json!({
        "source": "chat_feedback",
        "requestedBy": user_id.to_string(),
        "userComment": comment,
        "aiResponsePreview": ai_response_preview,
        "userQuestionPreview": preceding_user_message.as_deref().unwrap_or("").chars().take(300).collect::<String>(),
    }))
    .execute(&mpool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::project_db_routes::register_entity_routing(
        &state.db,
        "correction",
        correction_id,
        project_id,
    )
    .await;

    // Guard: se embedder/qdrant sono down, skip vettorializzazione (la correzione e' gia' in DB)
    let qdrant_ok = state
        .dependency_status
        .qdrant
        .load(std::sync::atomic::Ordering::Relaxed);
    let embedder_ok = state
        .dependency_status
        .embedder
        .load(std::sync::atomic::Ordering::Relaxed);
    if !qdrant_ok || !embedder_ok {
        tracing::info!(
            "corrections: skip vettorializzazione (qdrant={}, embedder={})",
            qdrant_ok,
            embedder_ok
        );
    } else {
        let vector = state
            .orchestrator
            .embed_text(&embed_input)
            .await
            .map_err(|e| api_error(StatusCode::BAD_GATEWAY, e.to_string()))?;
        vector_memory::upsert_prompt_correction_point(
            &state.db,
            &point_id,
            &vector,
            json!({
                "project_id": project_id.to_string(),
                "correction_id": correction_id.to_string(),
                "feedback_id": feedback_id.to_string(),
                "intent": intent,
                "provider": provider,
                "model": model,
                "text": correction_text,
                "active": true,
                "status": "open",
                "created_at": Utc::now().to_rfc3339(),
                "normalized_hint_hash": normalized_hash,
            }),
        )
        .await
        .map_err(|e| api_error(StatusCode::BAD_GATEWAY, e.to_string()))?;
    }

    let dedup_count = dedup_on_write(
        &state.db,
        project_id,
        &intent,
        &normalized_hash,
        correction_id,
    )
    .await?;
    let learning_action =
        apply_project_learning(&state.db, project_id, user_id, Some(&intent), false).await?;

    Ok(Json(json!({
        "ok": true,
        "feedbackId": feedback_id.to_string(),
        "correctionId": correction_id.to_string(),
        "deduplicatedCount": dedup_count,
        "learning": learning_action
    })))
}
/// Handler feedback positivo (pollice su): conferma esplicita che la risposta AI e' corretta.
///
/// A differenza di `feedback_error`:
/// - registra in `ai_response_feedback` con `feedback_type='positive'`
/// - NON genera `prompt_corrections` ne' embedding Qdrant (positivo = "lascia tutto com'e'")
/// - rinforza il Q-value con reward=1.0 sul `NexusBridge` se il messaggio ha run_id + intent
pub async fn feedback_positive(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
    Json(body): Json<FeedbackPositiveRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;
    let comment = body
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");

    // Separazione DB: endpoint keyed solo dal message_id -> pool del progetto via
    // directory di routing (fallback ricerca). DB non disponibile -> 503.
    let mpool =
        crate::project_db_routes::project_data_pool_by_message_from(&state.db, message_id).await?;

    let row = sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.metadata,
            s.user_id
        FROM chat_messages m
        JOIN chat_sessions s ON s.id = m.session_id
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_optional(&mpool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"));
    };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    if owner != Some(user_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Messaggio non accessibile",
        ));
    }

    let role: String = row.try_get("role").unwrap_or_default();
    if role != "assistant" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il feedback positivo e' consentito solo sui messaggi AI",
        ));
    }

    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let metadata: Value = row.try_get("metadata").unwrap_or_else(|_| json!({}));
    let intent = metadata
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .to_lowercase();
    let provider = metadata
        .get("provider")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let model = metadata
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let raw_run_id = metadata
        .get("runId")
        .or_else(|| metadata.get("agentRunId"))
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    // Verifica esistenza in orchestrator_runs (FK target).
    // I run agent moderni vivono in `agent_runs`, ma la FK del feedback punta
    // a `orchestrator_runs`. Se l'ID non esiste li', settiamo NULL invece di
    // far fallire l'insert (la colonna ammette NULL).
    let run_id: Option<Uuid> = match raw_run_id {
        Some(id) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM orchestrator_runs WHERE id = $1)")
                    .bind(id)
                    .fetch_one(&mpool)
                    .await
                    .unwrap_or(false);
            if exists {
                Some(id)
            } else {
                None
            }
        }
        None => None,
    };
    let agent_type_hint = metadata
        .get("agentType")
        .or_else(|| metadata.get("profile"))
        .and_then(Value::as_str)
        .unwrap_or("chat_default")
        .to_string();

    // Idempotenza: se gia' esiste un feedback positivo per questo messaggio, ritorna quello.
    let existing: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM ai_response_feedback
        WHERE message_id = $1 AND user_id = $2 AND feedback_type = 'positive'
        LIMIT 1
        "#,
    )
    .bind(message_id)
    .bind(user_id)
    .fetch_optional(&mpool)
    .await
    .unwrap_or(None);

    if let Some(existing_id) = existing {
        return Ok(Json(json!({
            "ok": true,
            "feedbackId": existing_id.to_string(),
            "alreadyRecorded": true,
            "newQValue": null,
        })));
    }

    let feedback_id = Uuid::new_v4();
    // `error_comment` e' NOT NULL nello schema: salva commento utente o sentinel.
    let comment_to_store = if comment.is_empty() {
        "[positive feedback senza commento]".to_string()
    } else {
        comment.to_string()
    };
    sqlx::query(
        r#"
        INSERT INTO ai_response_feedback (
            id, project_id, session_id, message_id, orchestrator_run_id, user_id,
            feedback_type, intent, provider, model, error_comment, status, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'positive', $7, $8, $9, $10, 'resolved', NOW(), NOW())
        "#,
    )
    .bind(feedback_id)
    .bind(project_id)
    .bind(session_id)
    .bind(message_id)
    .bind(run_id)
    .bind(user_id)
    .bind(&intent)
    .bind(&provider)
    .bind(&model)
    .bind(&comment_to_store)
    .execute(&mpool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::project_db_routes::register_entity_routing(
        &state.db,
        "feedback",
        feedback_id,
        project_id,
    )
    .await;

    // Rinforza Q-learning: reward=1.0 (successo confermato dall'utente).
    let mut new_q_value: Option<f32> = None;
    if let Some(bridge) = crate::nexus_bridge::NexusBridge::global() {
        let task_id = run_id
            .map(|u| u.to_string())
            .unwrap_or_else(|| message_id.to_string());
        let pascal = crate::internal_learning::snake_to_pascal(&agent_type_hint);
        let agent_type = nexus_orchestrator::AgentType::from_name(&pascal);
        let q = bridge.record_outcome(
            &task_id, &intent, agent_type, true, // success
            1.0,  // reward massimo
            0,    // duration_ms non disponibile qui
            None,
        );
        new_q_value = Some(q);
        tracing::info!(
            "feedback_positive: Q-update task={} intent={} agent={} new_q={}",
            task_id,
            intent,
            pascal,
            q,
        );
    }

    Ok(Json(json!({
        "ok": true,
        "feedbackId": feedback_id.to_string(),
        "alreadyRecorded": false,
        "newQValue": new_q_value,
    })))
}
pub async fn legacy_chat(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<LegacyChatRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&body.project_id)?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    // Separazione DB: chat_sessions e' migrata -> pool del progetto risolto una
    // volta per ricerca E creazione. Sul meta la ricerca rispondeva sempre
    // vuoto e ogni chiamata legacy creava una NUOVA sessione.
    let session_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
    let existing_session = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT id
        FROM chat_sessions
        WHERE project_id = $1
          AND user_id = $2
        ORDER BY updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(&session_pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let session_id = if let Some(session_id) = existing_session {
        session_id
    } else {
        let new_session_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO chat_sessions (id, project_id, user_id, title, status, created_at, updated_at)
            VALUES ($1, $2, $3, 'Nuova sessione', 'active', NOW(), NOW())
            "#,
        )
        .bind(new_session_id)
        .bind(project_id)
        .bind(user_id)
        .execute(&session_pool)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        new_session_id
    };

    let user_message_id = insert_message(
        &state.db,
        session_id,
        project_id,
        "user",
        &body.message,
        json!({
            "automationMode": "confirm",
            "attachments": [],
        }),
        None,
    )
    .await?;

    let (assistant_message, _) = run_turn(
        &state,
        user_id,
        session_id,
        project_id,
        body.profile_id.clone(),
        body.message.clone(),
        user_message_id,
        body.active_files.clone(),
        None,
        None,
        None,
        AutomationMode::Confirm,
        Vec::new(),
    )
    .await?;

    Ok(Json(json!({
        "content": assistant_message.content,
        "provider": assistant_message.provider,
        "model": assistant_message.model,
        "tokens_used": assistant_message.total_tokens.unwrap_or(0),
        "prompt_tokens": assistant_message.prompt_tokens.unwrap_or(0),
        "completion_tokens": assistant_message.completion_tokens.unwrap_or(0),
        "total_tokens": assistant_message.total_tokens.unwrap_or(0),
        "total_cost": assistant_message.total_cost.unwrap_or(0.0),
        "currency": assistant_message.currency.unwrap_or_else(|| "EUR".to_string()),
        "quota_status": "ok",
        "session_id": session_id.to_string(),
        "request_message_id": user_message_id.to_string(),
        "assistant_message_id": assistant_message.id,
    })))
}

// ── Pre-check messaggio ────────────────────────────────────────────────────
// Analizza un messaggio prima dell'invio: rileva errori ortografici/grammaticali
// e segnala richieste troppo vaghe che richiederebbero contesto aggiuntivo.
// Usa un modello economico/veloce (gpt-4.1-nano) con risposta JSON stretta.

#[derive(Debug, Deserialize)]
pub struct PrecheckRequest {
    pub message: String,
    /// Sessione corrente: se presente, il precheck riceve la cronologia
    /// recente per valutare il messaggio in contesto. Senza, valuta in
    /// isolamento (comportamento pre-fix, marca contestuali come generici).
    #[serde(default, alias = "sessionId")]
    pub session_id: Option<Uuid>,
}
pub async fn precheck_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<PrecheckRequest>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let message = body.message.trim();

    // Non fare il precheck per messaggi molto brevi
    if message.len() < 15 || message.split_whitespace().count() < 3 {
        return Ok(Json(json!({
            "ok": true, "correctedText": null,
            "contextSuggestion": null, "issues": [], "reason": null
        })));
    }

    // Non fare il precheck se sembra codice
    let looks_like_code = message.contains('`')
        || message.contains("```")
        || message.starts_with('/')
        || message.contains("./")
        || message.contains(":\\");
    if looks_like_code {
        return Ok(Json(json!({
            "ok": true, "correctedText": null,
            "contextSuggestion": null, "issues": [], "reason": null
        })));
    }

    let system_prompt = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "chat.precheck_message",
    )
    .await;

    // Arricchimento contestuale: se il client passa session_id, il precheck
    // riceve gli ultimi turni della conversazione. Risolve i falsi-positivi
    // su follow-up contestuali (es. "riepiloga gli animali" dopo una chat
    // sugli animali) che in isolamento sembrano "troppo generici" ma in
    // contesto sono chiarissimi.
    let effective_message = if let Some(sid) = body.session_id {
        build_message_with_recent_context_for_classifier(&state.db, sid, message).await
    } else {
        message.to_string()
    };

    let messages_json = serde_json::to_string(&json!([
        { "role": "user", "content": effective_message }
    ]))
    .unwrap_or_default();

    // Modello purpose-specific risolto dal PUNTO UNICO tier-only (regola L/G).
    let (provider_pf, model_pf) =
        crate::internal_routing::resolve_purpose_model(&state, "chat_feedback_generator")
            .await
            .into_model("chat_feedback_generator")
            .map_err(|m| api_error(StatusCode::SERVICE_UNAVAILABLE, m))?;
    let raw = match state
        .orchestrator
        .neural
        .generate_agent_turn(
            &provider_pf,
            &model_pf,
            &messages_json,
            "[]",
            300,
            &system_prompt,
        )
        .await
    {
        Ok(val) => val
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        Err(_) => {
            // Se il modello non risponde non bloccare l'utente
            return Ok(Json(json!({
                "ok": true, "correctedText": null,
                "contextSuggestion": null, "issues": [], "reason": null
            })));
        }
    };

    // Estrae il JSON anche se il modello ha aggiunto testo prima/dopo
    let json_start = raw.find('{').unwrap_or(0);
    let json_end = raw.rfind('}').map(|i| i + 1).unwrap_or(raw.len());
    let parsed: Value = serde_json::from_str(&raw[json_start..json_end]).unwrap_or_else(|_| {
        json!({
            "ok": true, "correctedText": null,
            "contextSuggestion": null, "issues": [], "reason": null
        })
    });

    let ok = parsed.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let corrected_text = parsed
        .get("correctedText")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        // Scarta solo se esattamente identico (byte-by-byte) o vuoto — non usare to_lowercase()
        // perché perderebbe correzioni su accenti o caratteri speciali
        .filter(|c| !c.trim().is_empty() && c.trim() != message.trim());
    let context_suggestion = parsed
        .get("contextSuggestion")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|s| !s.trim().is_empty());
    let issues: Vec<String> = parsed
        .get("issues")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let reason = parsed
        .get("reason")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    // ok=false solo se c'è davvero qualcosa di utile da mostrare
    let effective_ok =
        if corrected_text.is_none() && context_suggestion.is_none() && issues.is_empty() {
            true
        } else {
            ok
        };

    Ok(Json(json!({
        "ok": effective_ok,
        "correctedText": corrected_text,
        "contextSuggestion": context_suggestion,
        "issues": issues,
        "reason": reason
    })))
}

// ---------------------------------------------------------------------------
// POST /api/chat/feedback-assist
// Aiuta l'utente a formulare una descrizione precisa dell'anomalia nella
// risposta AI. Usa un modello economico; restituisce il testo suggerito.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackAssistRequest {
    /// Contenuto della risposta AI problematica (può essere troncato)
    pub message_content: String,
    /// Descrizione parziale già scritta dall'utente (può essere vuota)
    #[serde(default)]
    pub partial_description: String,
}
pub async fn feedback_assist_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<FeedbackAssistRequest>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;

    let system_prompt = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "chat.feedback_assist",
    )
    .await;

    if system_prompt.is_empty() {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Template non disponibile".to_string(),
        ));
    }

    // Tronca il contenuto del messaggio per non eccedere il contesto
    let msg_preview: String = body.message_content.chars().take(1200).collect();
    let partial = body.partial_description.trim().to_string();

    let user_content = if partial.is_empty() {
        format!("RISPOSTA AI:\n{}", msg_preview)
    } else {
        format!(
            "RISPOSTA AI:\n{}\n\nDESCRIZIONE PARZIALE DELL'UTENTE:\n{}",
            msg_preview, partial
        )
    };

    let messages_json = serde_json::to_string(&json!([
        { "role": "user", "content": user_content }
    ]))
    .unwrap_or_default();

    // Failover tier-aware (punto unico, regola L): il modello e' risolto dal
    // resolver a candidati; su fallimento (regola M: neural_value_is_failure —
    // include content vuoto e "[Error:...]") si prova il prossimo provider. Un
    // value di fallimento NON diventa MAI il suggerimento (era il leak
    // "[Error:...]" restituito come suggestion): AllCandidatesFailed -> "".
    let neural = &state.orchestrator.neural;
    let suggestion = {
        use crate::internal_routing::{
            complete_for_purpose_with_failover, AttemptOutcome, PurposeFailoverError,
        };
        let attempt = |prov: String, mdl: String| {
            let messages_json = &messages_json;
            let system_prompt = &system_prompt;
            async move {
                match neural
                    .generate_agent_turn(&prov, &mdl, messages_json, "[]", 400, system_prompt)
                    .await
                {
                    Ok(v) if !crate::orchestrator::neural_value_is_failure(&v) => {
                        AttemptOutcome::Done(
                            v.get("content")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .trim()
                                .trim_matches('"')
                                .to_string(),
                        )
                    }
                    Ok(_) => AttemptOutcome::Failover,
                    Err(e) => {
                        tracing::warn!("feedback_assist LLM error: {e}");
                        AttemptOutcome::Failover
                    }
                }
            }
        };
        match complete_for_purpose_with_failover(&state.db, "chat_title_generator", attempt).await {
            Ok(s) => s,
            Err(PurposeFailoverError::AllCandidatesFailed)
            | Err(PurposeFailoverError::NoCandidate(_)) => String::new(),
        }
    };

    Ok(Json(json!({ "suggestion": suggestion })))
}
