use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{
        api_error, ensure_project_access, parse_project_id, parse_user_id, ApiError, ApiResult,
    },
    AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionsQuery {
    pub project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatSessionRequest {
    pub project_id: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionContext {
    pub session_id: Uuid,
    pub project_id: Uuid,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSummary {
    id: String,
    project_id: String,
    title: String,
    status: String,
    message_count: i64,
    last_message_at: Option<String>,
    last_message_preview: Option<String>,
    created_at: String,
    updated_at: String,
}

pub(crate) async fn load_session_context(
    db: &PgPool,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<SessionContext, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT id, project_id, user_id
        FROM chat_sessions
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(
            axum::http::StatusCode::NOT_FOUND,
            "Sessione chat non trovata",
        ));
    };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    if owner != Some(user_id) {
        return Err(api_error(
            axum::http::StatusCode::FORBIDDEN,
            "Sessione chat non accessibile",
        ));
    }

    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    ensure_project_access(db, user_id, project_id).await?;

    Ok(SessionContext {
        session_id,
        project_id,
    })
}

pub(crate) async fn update_user_active_project(state: &AppState, user_id: Uuid, project_id: Uuid) {
    let _ = sqlx::query(
        r#"
        INSERT INTO project_open_sessions (
            id, user_id, project_id, last_opened_at, created_at, updated_at
        )
        VALUES (gen_random_uuid(), $1, $2, NOW(), NOW(), NOW())
        ON CONFLICT (user_id, project_id)
        DO UPDATE SET last_opened_at = NOW(), updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .execute(&state.db)
    .await;

    // Avvia indicizzazione semantica in background se non ancora eseguita.
    crate::projects::indexing::spawn_code_index_if_needed(state, project_id).await;
}

pub async fn list_chat_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ChatSessionsQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id_raw = query.project_id.as_deref().ok_or_else(|| {
        api_error(
            axum::http::StatusCode::BAD_REQUEST,
            "projectId e' obbligatorio",
        )
    })?;
    let project_id = parse_project_id(project_id_raw)?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let rows = sqlx::query(
        r#"
        SELECT
            s.id,
            s.project_id,
            s.title,
            s.status,
            s.created_at,
            s.updated_at,
            (
                SELECT COUNT(*)
                FROM chat_messages m
                WHERE m.session_id = s.id
                  AND m.deleted_at IS NULL
            ) AS message_count,
            (
                SELECT m.created_at
                FROM chat_messages m
                WHERE m.session_id = s.id
                  AND m.deleted_at IS NULL
                ORDER BY m.created_at DESC
                LIMIT 1
            ) AS last_message_at,
            (
                SELECT LEFT(m.content, 180)
                FROM chat_messages m
                WHERE m.session_id = s.id
                  AND m.deleted_at IS NULL
                ORDER BY m.created_at DESC
                LIMIT 1
            ) AS last_message_preview
        FROM chat_sessions s
        WHERE s.project_id = $1
          AND s.user_id = $2
        ORDER BY COALESCE(
            (
                SELECT m.created_at
                FROM chat_messages m
                WHERE m.session_id = s.id
                ORDER BY m.created_at DESC
                LIMIT 1
            ),
            s.updated_at
        ) DESC
        LIMIT 50
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let sessions = rows
        .iter()
        .filter_map(|row| {
            let id: Uuid = row.try_get("id").ok()?;
            let project_id: Uuid = row.try_get("project_id").ok()?;
            let title: String = row.try_get("title").ok()?;
            let status: String = row.try_get("status").ok()?;
            let message_count: i64 = row.try_get("message_count").ok()?;
            let last_message_at: Option<DateTime<Utc>> = row.try_get("last_message_at").ok()?;
            let last_message_preview: Option<String> = row.try_get("last_message_preview").ok()?;
            let created_at: DateTime<Utc> = row.try_get("created_at").ok()?;
            let updated_at: DateTime<Utc> = row.try_get("updated_at").ok()?;
            Some(SessionSummary {
                id: id.to_string(),
                project_id: project_id.to_string(),
                title,
                status,
                message_count,
                last_message_at: last_message_at.map(|value| value.to_rfc3339()),
                last_message_preview,
                created_at: created_at.to_rfc3339(),
                updated_at: updated_at.to_rfc3339(),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "sessions": sessions })))
}

pub async fn create_chat_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateChatSessionRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&body.project_id)?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let session_id = Uuid::new_v4();
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "Nuova sessione".to_string());

    sqlx::query(
        r#"
        INSERT INTO chat_sessions (id, project_id, user_id, title, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, 'active', NOW(), NOW())
        "#,
    )
    .bind(session_id)
    .bind(project_id)
    .bind(user_id)
    .bind(&title)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    update_user_active_project(&state, user_id, project_id).await;

    Ok(Json(json!({
        "session": {
            "id": session_id.to_string(),
            "projectId": project_id.to_string(),
            "title": title,
            "status": "active",
        }
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameChatSessionRequest {
    pub title: String,
}

pub async fn rename_chat_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id_str): Path<String>,
    Json(body): Json<RenameChatSessionRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id_str)
        .map_err(|_| api_error(axum::http::StatusCode::BAD_REQUEST, "session id non valido"))?;

    let ctx = load_session_context(&state.db, session_id, user_id).await?;

    let title = body.title.trim().to_string();
    if title.is_empty() {
        return Err(api_error(
            axum::http::StatusCode::BAD_REQUEST,
            "Il titolo non puo' essere vuoto",
        ));
    }

    sqlx::query("UPDATE chat_sessions SET title = $1, updated_at = NOW() WHERE id = $2")
        .bind(&title)
        .bind(ctx.session_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true, "title": title })))
}

pub async fn delete_chat_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id_str): Path<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id_str)
        .map_err(|_| api_error(axum::http::StatusCode::BAD_REQUEST, "session id non valido"))?;

    let ctx = load_session_context(&state.db, session_id, user_id).await?;

    // Soft-delete messages
    sqlx::query(
        "UPDATE chat_messages SET deleted_at = NOW() WHERE session_id = $1 AND deleted_at IS NULL",
    )
    .bind(ctx.session_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Hard-delete session
    sqlx::query("DELETE FROM chat_sessions WHERE id = $1")
        .bind(ctx.session_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

/// Esito di una compattazione riuscita. Riusato sia dall'endpoint manuale
/// (`compact_chat_session`) sia dall'auto-compact a soglia (`spawn_agent_run`).
#[derive(Debug, Clone)]
pub(crate) struct CompactOutcome {
    pub summary_text: String,
    pub point_id: String,
    /// Numero di messaggi user/assistant soft-deletati dal compact.
    pub soft_deleted: u64,
}

/// Errore di compattazione che trasporta lo status HTTP coerente con il
/// comportamento storico dell'endpoint manuale. L'auto-compact lo tratta come
/// stringa best-effort (logga WARN e prosegue).
#[derive(Debug, Clone)]
pub(crate) struct CompactError {
    pub status: axum::http::StatusCode,
    pub message: String,
}

impl std::fmt::Display for CompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl CompactError {
    fn new(status: axum::http::StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

/// Logica core di compattazione, condivisa tra l'endpoint manuale e
/// l'auto-compact a soglia. Genera il riassunto, lo salva in vector memory
/// (Qdrant + prompt_corrections), fa soft-delete dei messaggi user/assistant,
/// inserisce il messaggio role='summary', marca la sessione 'compacted' ed
/// emette gli eventi SSE. Niente auth qui: il chiamante deve aver gia'
/// verificato l'accesso (l'endpoint via load_session_context, l'auto-compact
/// perche' opera nel contesto del run gia' autorizzato).
pub(crate) async fn compact_session_core(
    state: &AppState,
    session_id: Uuid,
    project_id: Uuid,
) -> Result<CompactOutcome, CompactError> {
    // La compattazione e' sempre ripetibile: ogni invocazione produce un nuovo
    // riassunto + nuovo correction_id, e ri-marca la sessione come 'compacted'.
    // Necessario perche' dopo una compattazione l'utente continua a chattare
    // accumulando nuovi token che devono poter essere a loro volta compattati.

    // Load non-deleted messages
    let rows = sqlx::query(
        r#"
        SELECT role, content FROM chat_messages
        WHERE session_id = $1 AND deleted_at IS NULL
        ORDER BY created_at ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| CompactError::internal(e.to_string()))?;

    if rows.is_empty() {
        return Err(CompactError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "Nessun messaggio da compattare",
        ));
    }

    // Build messages JSON for summarization
    let mut msgs: Vec<serde_json::Value> = rows
        .iter()
        .filter_map(|row| {
            let role: String = row.try_get("role").ok()?;
            let content: String = row.try_get("content").ok()?;
            Some(json!({ "role": role, "content": content }))
        })
        .collect();

    // Append summarization instruction
    msgs.push(json!({
        "role": "user",
        "content": "Riassumi questa conversazione estraendo: le decisioni chiave prese, i cambiamenti al codice effettuati, i contesti e le conoscenze apprese utili per il progetto. Sii conciso e strutturato con bullet points."
    }));

    let messages_json =
        serde_json::to_string(&msgs).map_err(|e| CompactError::internal(e.to_string()))?;

    // Risolvi provider/modello dal PUNTO UNICO resolve_purpose_model (regola G +
    // regola L): niente modello hardcoded, niente fallback a un provider morto.
    // Rispetta tier-rule, cooldown, disponibilita'. Se nessun provider e'
    // disponibile, propaga errore chiaro all'utente.
    let (summary_provider, summary_model) = {
        use crate::internal_routing::{resolve_purpose_model, PurposeResolution};
        match resolve_purpose_model(state, "conversation_summary").await {
            PurposeResolution::Resolved {
                provider, model, ..
            } => (provider, model),
            PurposeResolution::NoCapableModel { tier } => {
                return Err(CompactError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    format!(
                        "Compattazione non disponibile: nessun modello del tier '{tier}' e' \
                         disponibile (capability mancante o provider in cooldown). \
                         Riprova piu' tardi o ricarica il credito del provider."
                    ),
                ));
            }
            PurposeResolution::NotFound => {
                return Err(CompactError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "Compattazione non disponibile: purpose 'conversation_summary' \
                     non configurato (o privo di tier) in nexus_purpose_model. Aggiungi la \
                     configurazione dall'admin panel.",
                ));
            }
            PurposeResolution::MatrixUnavailable(e) => {
                return Err(CompactError::internal(format!(
                    "Compattazione non disponibile: routing matrix irraggiungibile ({e})"
                )));
            }
        }
    };

    let summary_resp = state
        .orchestrator
        .neural
        .generate_agent_turn(
            &summary_provider,
            &summary_model,
            &messages_json,
            "[]",
            1500,
            "",
        )
        .await
        .map_err(|e| CompactError::internal(format!("Neural Core error: {e}")))?;

    // Extract text from response (try multiple possible keys)
    let summary_text = summary_resp
        .get("content")
        .and_then(|v| v.as_str())
        .or_else(|| summary_resp.get("text").and_then(|v| v.as_str()))
        .or_else(|| summary_resp.get("message").and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();

    // Detection riassunto degenere: il neural service ritorna sempre 200 anche
    // quando tutti i provider falliscono, con `content` tipo "[Error: ...]" o
    // "Errore del provider AI. Controlla i log per i dettagli." (vedi
    // error_handler.py). Questi NON sono riassunti validi: trattali come
    // failure cosi' il frontend mostra errore esplicito invece di salvare un
    // "riassunto" inutile che spreca lo slot della session memory.
    let lower = summary_text.to_lowercase();
    let is_degenerate = summary_text.is_empty()
        || summary_text.len() < 40
        || lower.starts_with("[error")
        || lower.starts_with("errore del provider")
        || lower.contains("controlla i log per i dettagli")
        || lower.contains("nessun provider")
        || lower.contains("billing");
    if is_degenerate {
        tracing::warn!(
            "compact_session_core: riassunto degenere ({} char): \"{}\"",
            summary_text.len(),
            summary_text.chars().take(120).collect::<String>()
        );
        return Err(CompactError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Compattazione fallita: tutti i provider AI hanno restituito errore. \
             Riprova fra qualche minuto o sblocca un provider in /admin/settings/providers.",
        ));
    }

    // Embed the summary — guard: se dipendenze vettoriali down, skip (non bloccante)
    let qdrant_ok = state
        .dependency_status
        .qdrant
        .load(std::sync::atomic::Ordering::Relaxed);
    let embedder_ok = state
        .dependency_status
        .embedder
        .load(std::sync::atomic::Ordering::Relaxed);
    let point_id = Uuid::new_v4().to_string();
    if !qdrant_ok || !embedder_ok {
        tracing::info!(
            "chat_sessions: skip embed summary (qdrant={}, embedder={})",
            qdrant_ok,
            embedder_ok
        );
    } else {
        let vector = state
            .orchestrator
            .embed_text(&summary_text)
            .await
            .map_err(|e| CompactError::internal(format!("Embedding error: {e}")))?;

        // Store in Qdrant via vector_memory
        let payload = json!({
            "project_id": project_id.to_string(),
            "session_id": session_id.to_string(),
            "type": "session_memory",
            "active": false,   // inactive until user activates
            "text": summary_text,
        });
        crate::vector_memory::upsert_prompt_correction_point(
            &state.db, &point_id, &vector, payload,
        )
        .await
        .map_err(|e| CompactError::internal(e.to_string()))?;
    }

    // Persist to prompt_corrections table
    let correction_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO prompt_corrections
            (id, project_id, session_id, intent, correction_text,
             normalized_hint_hash, qdrant_point_id, active, status, type)
        VALUES ($1, $2, $3, 'session_memory', $4, $5, $6, false, 'saved', 'session_memory')
    "#,
    )
    .bind(correction_id)
    .bind(project_id)
    .bind(session_id)
    .bind(&summary_text)
    .bind(format!("session:{}", session_id))
    .bind(&point_id)
    .execute(&state.db)
    .await
    .map_err(|e| CompactError::internal(e.to_string()))?;

    // Soft-delete dei messaggi user/assistant precedenti il compact: la
    // sintesi e' ora in vector memory e verra' iniettata come messaggio
    // 'summary' nuovo (sotto). Questo riduce davvero il contesto inviato alle
    // chiamate LLM successive (build_recent_conversation_history filtra
    // deleted_at IS NULL) e fa scendere la TokenUsageBar nella UI.
    let soft_deleted = sqlx::query(
        "UPDATE chat_messages SET deleted_at = NOW() \
         WHERE session_id = $1 AND deleted_at IS NULL \
           AND role IN ('user', 'assistant')",
    )
    .bind(session_id)
    .execute(&state.db)
    .await
    .map_err(|e| CompactError::internal(e.to_string()))?;

    // Stima del costo "logico" del summary in token (approx 4 char per token).
    let summary_tokens_est: i64 = ((summary_text.chars().count() as i64) / 4).max(1);

    // Inserisce un messaggio role='summary' nel thread, in modo che
    // build_recent_conversation_history lo carichi come primo messaggio nelle
    // chiamate LLM future (deve essere whitelisted nel filtro role).
    let summary_metadata = serde_json::json!({
        "isCompactSummary": true,
        "qdrantPointId": point_id,
        "totalTokens": summary_tokens_est,
        "totalCost": 0.0,
        "softDeletedCount": soft_deleted.rows_affected(),
    });
    let summary_msg_id = Uuid::new_v4();
    let summary_content = format!(
        "[Riassunto della conversazione precedente — generato al compact]\n\n{}",
        summary_text
    );
    sqlx::query(
        "INSERT INTO chat_messages (id, session_id, project_id, role, content, metadata, created_at) \
         VALUES ($1, $2, $3, 'summary', $4, $5, NOW())",
    )
    .bind(summary_msg_id)
    .bind(session_id)
    .bind(project_id)
    .bind(&summary_content)
    .bind(&summary_metadata)
    .execute(&state.db)
    .await
    .map_err(|e| CompactError::internal(e.to_string()))?;

    // Mark session as compacted
    sqlx::query("UPDATE chat_sessions SET status = 'compacted', updated_at = NOW() WHERE id = $1")
        .bind(session_id)
        .execute(&state.db)
        .await
        .map_err(|e| CompactError::internal(e.to_string()))?;

    // Dopo il soft-delete, il totale TOKEN mostrato dalla TokenUsageBar e' solo
    // quello del summary nuovo (i precedenti sono deleted_at NOT NULL e la query
    // frontend li filtra) — corretto per il calcolo del context window %.
    let total_tokens: i64 = summary_tokens_est;
    // Il COSTO totale della chat e' invece CUMULATIVO: i turni appena
    // soft-deletati dalla compattazione sono stati comunque PAGATI, quindi il
    // costo non va mai azzerato. Sommiamo il costo di TUTTI i messaggi assistant
    // della sessione (inclusi i soft-deleted). Bug storico: qui era hardcodato a
    // 0.0, percio' compattando una chat si perdeva il costo totale speso.
    let total_cost_usd: f64 = sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(SUM((metadata->>'totalCost')::float8), 0.0)::float8 \
         FROM chat_messages \
         WHERE session_id = $1 AND role = 'assistant' \
           AND metadata->>'totalCost' IS NOT NULL",
    )
    .bind(session_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0.0);

    // Emette eventi dispatcher: il frontend ascolta via SSE e ricalcola UI.
    // - ChatSessionCompacted → use-chat aggiorna tokenUsage
    // - ChatSessionStatusChanged → tab della sessione mostra icona "compactata"
    nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::ChatSessionCompacted {
            session_id,
            summary_point_id: Some(point_id.clone()),
            total_tokens,
            total_cost_usd,
        },
    );
    nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::ChatSessionStatusChanged {
            session_id,
            status: "compacted".into(),
        },
    );

    Ok(CompactOutcome {
        summary_text,
        point_id,
        soft_deleted: soft_deleted.rows_affected(),
    })
}

/// Endpoint manuale (pulsante "Compatta chat"). Thin wrapper: verifica
/// l'accesso e delega alla logica core condivisa con l'auto-compact.
pub async fn compact_chat_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id_str): Path<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id_str)
        .map_err(|_| api_error(axum::http::StatusCode::BAD_REQUEST, "session id non valido"))?;

    let ctx = load_session_context(&state.db, session_id, user_id).await?;

    let outcome = compact_session_core(&state, ctx.session_id, ctx.project_id)
        .await
        .map_err(|e| api_error(e.status, e.message))?;

    Ok(Json(json!({
        "ok": true,
        "summary": outcome.summary_text,
        "pointId": outcome.point_id,
    })))
}

pub async fn list_project_memories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(project_id_str): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id_str)
        .map_err(|_| api_error(axum::http::StatusCode::BAD_REQUEST, "project id non valido"))?;

    let rows = sqlx::query(
        r#"
        SELECT
            pc.id, pc.session_id, pc.correction_text, pc.active, pc.created_at,
            cs.title as session_title
        FROM prompt_corrections pc
        LEFT JOIN chat_sessions cs ON cs.id = pc.session_id
        WHERE pc.project_id = $1 AND pc.type = 'session_memory'
          AND pc.deleted_at IS NULL
        ORDER BY pc.created_at DESC
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let memories: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let id: Uuid = row.try_get("id").unwrap_or_default();
            let session_id: Option<Uuid> = row.try_get("session_id").ok().flatten();
            let text: String = row.try_get("correction_text").unwrap_or_default();
            let active: bool = row.try_get("active").unwrap_or(false);
            let created_at: DateTime<Utc> = row.try_get("created_at").unwrap_or_default();
            let session_title: Option<String> = row.try_get("session_title").ok().flatten();
            json!({
                "id": id,
                "sessionId": session_id,
                "sessionTitle": session_title.unwrap_or_else(|| "Sessione rimossa".to_string()),
                "summary": text,
                "active": active,
                "createdAt": created_at,
            })
        })
        .collect();

    Ok(Json(json!({ "memories": memories })))
}

pub async fn toggle_project_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(memory_id_str): Path<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let memory_id = Uuid::parse_str(&memory_id_str)
        .map_err(|_| api_error(axum::http::StatusCode::BAD_REQUEST, "memory id non valido"))?;

    // Flip the active flag
    let new_active: bool = sqlx::query_scalar(
        "UPDATE prompt_corrections SET active = NOT active, updated_at = NOW()
         WHERE id = $1 AND type = 'session_memory'
         RETURNING active",
    )
    .bind(memory_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync to Qdrant: retrieve point_id, then update payload
    let qdrant_point_id: String =
        sqlx::query_scalar("SELECT qdrant_point_id FROM prompt_corrections WHERE id = $1")
            .bind(memory_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("toggle prompt correction: SELECT qdrant_point_id fallita: {e}");
                String::new()
            });

    if !qdrant_point_id.is_empty() {
        // Best-effort Qdrant payload update — ignore errors (DB is source of truth)
        let _ =
            crate::vector_memory::set_point_active(&state.db, &qdrant_point_id, new_active).await;
    }

    Ok(Json(json!({ "ok": true, "active": new_active })))
}
