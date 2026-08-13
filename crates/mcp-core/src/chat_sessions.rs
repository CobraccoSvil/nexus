use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use crate::project_db_routes::EntityKind;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{
        api_error, ensure_project_access, parse_project_id, parse_user_id, ApiError, ApiResult,
    },
    AppState,
};

/// Quanto deve essere lungo il riassunto VISIBILE della compattazione. Il tetto
/// da mandare lo calcola il catalogo (`tetto_output`): qui si dichiara solo cio'
/// che si deve poter leggere.
const RIASSUNTO: nexus_agent_graph::decisions::tetto_output::RichiestaOutput =
    nexus_agent_graph::decisions::tetto_output::RichiestaOutput::Visibile(1500);

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
    /// Pin provider/modello per-sessione (chat_sessions.preferred_provider /
    /// preferred_model): il frontend re-idrata il dropdown della chat da qui,
    /// cosi' il pin sopravvive al refresh della pagina.
    preferred_provider: Option<String>,
    preferred_model: Option<String>,
}

pub(crate) async fn load_session_context(
    state: &AppState,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<SessionContext, ApiError> {
    // La sessione chat vive nel DB del progetto (risolto da session_id via la
    // directory di routing); ensure_project_access resta sul meta-DB (globale).
    // DB progetto non disponibile -> 503 strutturato (regola M), mai meta-DB.
    let chat_pool =
        crate::project_db_routes::project_data_pool_by_session(state, session_id).await?;
    let row = sqlx::query(
        r#"
        SELECT id, project_id, user_id
        FROM chat_sessions
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .fetch_optional(&chat_pool)
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
    ensure_project_access(&state.db, user_id, project_id).await?;

    Ok(SessionContext {
        session_id,
        project_id,
    })
}

pub(crate) async fn update_user_active_project(state: &AppState, user_id: Uuid, project_id: Uuid) {
    // Separazione DB: project_open_sessions e' migrata, instrada sul pool del
    // progetto. Best-effort: DB progetto non disponibile -> WARN e si salta
    // l'aggiornamento (mai scriverlo sul meta, regola M).
    match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
        Ok(data_pool) => {
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
            .execute(&data_pool)
            .await;
        }
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                "update_user_active_project: DB progetto non disponibile, last_opened_at non aggiornato"
            );
        }
    }

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

    // Separazione DB (regola L): i dati chat vivono nel DB del progetto;
    // project_data_pool e' il punto unico. DB non disponibile (es. progetto in
    // provisioning) -> 503 strutturato, MAI lista vuota dal meta (incidente
    // 2026-07-20: il client scambiava il fallback per "nessuna sessione").
    // ensure_project_access sopra resta sul meta-DB (membership globale).
    let chat_pool = crate::project_db_routes::project_data_pool(&state, project_id).await?;

    let rows = sqlx::query(
        r#"
        SELECT
            s.id,
            s.project_id,
            s.title,
            s.status,
            s.created_at,
            s.updated_at,
            s.preferred_provider,
            s.preferred_model,
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
    .fetch_all(&chat_pool)
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
            let preferred_provider: Option<String> = row.try_get("preferred_provider").ok()?;
            let preferred_model: Option<String> = row.try_get("preferred_model").ok()?;
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
                preferred_provider,
                preferred_model,
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

    // Separazione DB: la sessione chat si scrive nel DB del progetto. DB non
    // disponibile -> 503 (regola M): l'INSERT sul meta creava una sessione
    // "fantasma" che spariva dalla UI appena il DB del progetto tornava su.
    let chat_pool = crate::project_db_routes::project_data_pool(&state, project_id).await?;

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
    .execute(&chat_pool)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Directory di routing (sempre sul meta-DB, mig 0496): session -> project,
    // cosi' gli handler con solo session_id risolvono il pool del progetto anche
    // a flag on. Best-effort: il punto unico logga WARN, mai errore propagato.
    crate::project_db_routes::register_entity_routing(&state.db, EntityKind::Session, session_id, project_id)
        .await;

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
pub struct UpdateChatSessionRequest {
    /// Nuovo titolo. Assente = non toccare.
    pub title: Option<String>,
    /// Pin provider per-sessione dal dropdown della chat. Semantica:
    /// campo assente = non toccare; "auto" o stringa vuota = azzera (NULL,
    /// routing automatico); altro valore = pin. Punto unico di persistenza:
    /// chat_sessions.preferred_provider, la stessa colonna gia' usata dal
    /// comando testuale "usa <modello>" (verdetto di
    /// `model_switch::resolve_switch_verdict` sul segnale del classificatore)
    /// e letta da send_chat_message come override di default dei run successivi.
    pub preferred_provider: Option<String>,
    /// Come preferred_provider, per il modello.
    pub preferred_model: Option<String>,
}

/// Normalizza un valore di pin dal client: "auto" / stringa vuota significano
/// "nessuna preferenza" (NULL in chat_sessions).
fn normalize_session_pin(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub async fn update_chat_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id_str): Path<String>,
    Json(body): Json<UpdateChatSessionRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id_str)
        .map_err(|_| api_error(axum::http::StatusCode::BAD_REQUEST, "session id non valido"))?;

    let ctx = load_session_context(&state, session_id, user_id).await?;

    let title = match body.title.as_deref().map(str::trim) {
        Some("") => {
            return Err(api_error(
                axum::http::StatusCode::BAD_REQUEST,
                "Il titolo non puo' essere vuoto",
            ));
        }
        Some(value) => Some(value.to_string()),
        None => None,
    };
    let set_provider = body.preferred_provider.is_some();
    let provider_value = body
        .preferred_provider
        .as_deref()
        .and_then(normalize_session_pin);
    let set_model = body.preferred_model.is_some();
    let model_value = body
        .preferred_model
        .as_deref()
        .and_then(normalize_session_pin);

    let chat_pool = crate::project_db_routes::project_data_pool(&state, ctx.project_id).await?;
    sqlx::query(
        r#"
        UPDATE chat_sessions SET
            title = COALESCE($1, title),
            preferred_provider = CASE WHEN $2 THEN $3 ELSE preferred_provider END,
            preferred_model = CASE WHEN $4 THEN $5 ELSE preferred_model END,
            updated_at = NOW()
        WHERE id = $6
        "#,
    )
    .bind(&title)
    .bind(set_provider)
    .bind(&provider_value)
    .bind(set_model)
    .bind(&model_value)
    .bind(ctx.session_id)
    .execute(&chat_pool)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "ok": true,
        "title": title,
        "preferredProvider": provider_value,
        "preferredModel": model_value,
    })))
}

pub async fn delete_chat_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id_str): Path<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id_str)
        .map_err(|_| api_error(axum::http::StatusCode::BAD_REQUEST, "session id non valido"))?;

    let ctx = load_session_context(&state, session_id, user_id).await?;

    let chat_pool = crate::project_db_routes::project_data_pool(&state, ctx.project_id).await?;
    // Soft-delete messages
    sqlx::query(
        "UPDATE chat_messages SET deleted_at = NOW() WHERE session_id = $1 AND deleted_at IS NULL",
    )
    .bind(ctx.session_id)
    .execute(&chat_pool)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Hard-delete session
    sqlx::query("DELETE FROM chat_sessions WHERE id = $1")
        .bind(ctx.session_id)
        .execute(&chat_pool)
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
    /// Totali post-compact: inclusi nella risposta HTTP cosi' il frontend
    /// aggiorna la barra token in modo sincrono, senza dipendere dall'evento SSE
    /// ChatSessionCompacted (che puo' essere emesso con subscribers=0 e perdersi).
    pub total_tokens: i64,
    pub total_cost_usd: f64,
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
/// Sezione "stato lavori" DETERMINISTICA per il summary di compattazione.
///
/// Causa radice (incidente Beauty-Book 2026-06-11): il summary LLM del compact
/// aveva PERSO ogni traccia dell'estrazione figma gia' eseguita (parlava solo di
/// file di test e porte) -> ogni run successivo ripartiva cieco, ri-estraeva e
/// ri-esplorava da capo (9 run ridondanti, ~353K token). I fatti strutturali dei
/// run NON possono dipendere da cosa l'LLM decide di tenere: questa sezione e'
/// generata da query su agent_runs/agent_steps e APPESA al summary, sempre.
/// Stato lavori strutturale da appendere al riassunto di compattazione
/// (incidente Beauty-Book: i fatti dei run non vanno delegati alla memoria
/// dell'LLM). Punto unico (regola L): delega al worklog di sessione
/// (`session_worklog`), che gia' deriva deterministicamente file toccati,
/// esiti e tentativi falliti dagli `agent_steps` — niente seconda query
/// duplicata sugli stessi tool mutativi. `None` se il worklog e' vuoto o
/// disabilitato (il riassunto procede senza la sezione strutturale).
async fn structured_work_state(db: &sqlx::PgPool, session_id: Uuid) -> Option<String> {
    // Worklog di sessione nel DB del progetto (risolto da session_id via
    // routing). Best-effort: DB non disponibile -> il riassunto procede senza
    // la sezione strutturale (WARN), mai una lettura vuota dal meta.
    let pool = match crate::project_db_routes::project_data_pool_by_session_from(db, session_id)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "structured_work_state: DB progetto non disponibile, sezione worklog omessa"
            );
            return None;
        }
    };
    let block = crate::session_worklog::fetch_rendered_block(db, &pool, session_id).await?;
    Some(format!(
        "\n\n## Stato lavori (worklog di sessione, punto unico — non perdere questi fatti)\n{block}"
    ))
}

/// Il payload del punto vettoriale di una memoria di sessione: le voci del
/// pannello "Memoria del progetto".
///
/// E' una funzione, e non tre righe dentro [`compact_session_core`], perche' il
/// suo consumatore vero - il richiamo in `prompt_memories` - deve poter essere
/// misurato su QUESTA forma e non su una ricopiata a mano in un test (regola O).
/// Un test che si scrive da se' il payload resta verde anche quando produttore e
/// consumatore divergono, che e' il modo in cui il difetto dei contatori e'
/// rimasto invisibile.
///
/// La memoria nasce INATTIVA: entra nei prompt solo dopo che l'utente l'ha attivata
/// dal pannello, e il filtro della ricerca esige `active = true`.
pub(crate) fn payload_memoria_di_sessione(
    project_id: Uuid,
    session_id: Uuid,
    summary_text: &str,
) -> serde_json::Value {
    crate::vector_memory::prompt_correction_payload(
        project_id,
        summary_text,
        false,
        json!({
            "session_id": session_id.to_string(),
            "type": "session_memory",
        }),
    )
}

/// Verdetto su UN turno neural prodotto per il riassunto di compattazione:
/// tenerlo, o passare al prossimo candidato del tier.
///
/// Delega al PUNTO UNICO del "questo turno va ritentato"
/// (`orchestrator::neural_value_is_failure`, regole L e M): il fallimento tecnico
/// e' gia' dichiarato in forma STRUTTURATA dai due soli produttori del Value
/// neural — `agent_turn_value_from_gw` (`error` null) e
/// `error_agent_turn_from_error` (`error`/`error_class` non-null) — piu' il caso
/// del content vuoto. Il TESTO del riassunto non entra nel verdetto: un riassunto
/// che parla di errori, di billing o di provider mancanti e' un riassunto valido,
/// non un errore.
///
/// Il caso storico del "200 con output inutile" e' gia' chiuso ALLA FONTE: il
/// gateway riconosce la risposta degenere (`types::is_degenerate_completion`) e
/// la converte in `CallFailure::empty_completion`, quindi qui arriva come errore
/// tipizzato, non come prosa da annusare.
///
/// Esiste come funzione a se' per la regola O: il verdetto viveva dentro un
/// closure `async` di [`compact_session_core`], che pretende DB e gateway vivi;
/// nessun test poteva raggiungerlo per la strada della produzione.
fn esito_tentativo_riassunto(
    turn: serde_json::Value,
) -> crate::internal_routing::AttemptOutcome<serde_json::Value> {
    use crate::internal_routing::AttemptOutcome;
    if crate::orchestrator::neural_value_is_failure(&turn) {
        return AttemptOutcome::Failover;
    }
    AttemptOutcome::Done(turn)
}

pub(crate) async fn compact_session_core(
    state: &AppState,
    session_id: Uuid,
    project_id: Uuid,
) -> Result<CompactOutcome, CompactError> {
    // La compattazione e' sempre ripetibile: ogni invocazione produce un nuovo
    // riassunto + nuovo correction_id, e ri-marca la sessione come 'compacted'.
    // Necessario perche' dopo una compattazione l'utente continua a chattare
    // accumulando nuovi token che devono poter essere a loro volta compattati.

    // Separazione DB: i dati chat della compattazione vivono nel DB del
    // progetto. DB non disponibile -> errore con lo status del punto unico
    // (503), propagato sia all'endpoint manuale sia all'auto-compact.
    let chat_pool = crate::project_db_routes::project_data_pool(state, project_id)
        .await
        .map_err(|e| CompactError::new(e.status_code(), e.to_string()))?;

    // Load non-deleted messages
    let rows = sqlx::query(
        r#"
        SELECT role, content FROM chat_messages
        WHERE session_id = $1 AND deleted_at IS NULL
        ORDER BY created_at ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(&chat_pool)
    .await
    .map_err(|e| CompactError::internal(e.to_string()))?;

    if rows.is_empty() {
        return Err(CompactError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "Nessun messaggio da compattare",
        ));
    }

    // Build messages JSON for summarization. Normalizza il ruolo DB -> ruolo LLM
    // (punto unico, regola L): una RI-compattazione legge anche il messaggio
    // role='summary' del compact precedente; inviarlo grezzo al brain/gateway
    // causa "unknown variant `summary`" (i provider accettano solo
    // system/user/assistant/tool). 'summary' -> 'user'.
    let mut msgs: Vec<serde_json::Value> = rows
        .iter()
        .filter_map(|row| {
            let role: String = row.try_get("role").ok()?;
            let content: String = row.try_get("content").ok()?;
            let llm_role = crate::chat_messages::db_role_to_llm_role(&role);
            Some(json!({ "role": llm_role, "content": content }))
        })
        .collect();

    // Istruzione di compattazione. Con il gate strutturato (mig 0413) usa il
    // template DB system.session_compact_structured (output JSON: riassunto +
    // decisioni durature -> worklog); altrimenti il prompt legacy testuale.
    // Fallback al legacy se il template e' vuoto (regola H).
    let structured_compact =
        nexus_auth::get_setting(&state.db, "agent.worklog.compact_writes_decisions")
            .await
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(true);
    const LEGACY_COMPACT_PROMPT: &str = "Riassumi questa conversazione estraendo: le decisioni chiave prese, i cambiamenti al codice effettuati, i contesti e le conoscenze apprese utili per il progetto. Sii conciso e strutturato con bullet points.";
    let compact_instruction = if structured_compact {
        let tpl = nexus_types::get_template_or_default(
            &state.db,
            &state.template_cache,
            "system.session_compact_structured",
        )
        .await;
        if tpl.trim().is_empty() {
            LEGACY_COMPACT_PROMPT.to_string()
        } else {
            tpl
        }
    } else {
        LEGACY_COMPACT_PROMPT.to_string()
    };
    msgs.push(json!({ "role": "user", "content": compact_instruction }));

    let messages_json =
        serde_json::to_string(&msgs).map_err(|e| CompactError::internal(e.to_string()))?;

    // FAILOVER tier-aware (punto unico complete_for_purpose_with_failover, regola
    // L/G): niente modello hardcoded; risolve N candidati del tier e prova in
    // ordine, facendo failover al prossimo su fallimento (regola M:
    // neural_value_is_failure — include timeout/errore/content vuoto). Prima
    // degradava al PRIMO provider fallito; ora si arrende solo se TUTTI falliscono.
    let summary_resp = {
        use crate::internal_routing::{
            complete_for_purpose_with_failover, AttemptOutcome, PurposeFailoverError,
        };
        let neural = &state.orchestrator.neural;
        let attempt = |prov: String, mdl: String| {
            let messages_json = &messages_json;
            async move {
                match neural
                    .generate_agent_turn(&prov, &mdl, messages_json, "[]", RIASSUNTO, "")
                    .await
                {
                    Ok(v) => esito_tentativo_riassunto(v),
                    // Errore LOCALE (bridge non configurato, messages_json
                    // invalido): il turno non e' mai partito, prova il prossimo.
                    Err(_) => AttemptOutcome::Failover,
                }
            }
        };
        match complete_for_purpose_with_failover(&state.db, "conversation_summary", attempt).await {
            Ok(v) => v,
            Err(PurposeFailoverError::AllCandidatesFailed) => {
                return Err(CompactError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "Compattazione non disponibile: nessun provider del tier ha prodotto un \
                     riassunto valido. Riprova piu' tardi o ricarica il credito del provider."
                        .to_string(),
                ));
            }
            Err(PurposeFailoverError::NoCandidate(_)) => {
                return Err(CompactError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "Compattazione non disponibile: purpose 'conversation_summary' non \
                     configurato (o privo di tier) in nexus_purpose_model."
                        .to_string(),
                ));
            }
        }
    };

    // Extract text from response (try multiple possible keys)
    let summary_text = summary_resp
        .get("content")
        .and_then(|v| v.as_str())
        .or_else(|| summary_resp.get("text").and_then(|v| v.as_str()))
        .or_else(|| summary_resp.get("message").and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();

    // Parsing strutturato (mig 0413): se il gate e' attivo e il modello ha
    // prodotto JSON, usa summary_markdown come riassunto ed estrai le decisioni
    // durature per il worklog. Fallback al testo grezzo se non e' JSON (regola H).
    let mut distilled_decisions: Vec<String> = Vec::new();
    let summary_text = if structured_compact {
        match nexus_types::llm_json::extract_json_block(&summary_text) {
            Some(parsed) => {
                if let Some(arr) = parsed.get("decisions").and_then(|v| v.as_array()) {
                    distilled_decisions = arr
                        .iter()
                        .filter_map(|d| d.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                parsed
                    .get("summary_markdown")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(summary_text)
            }
            None => summary_text,
        }
    } else {
        summary_text
    };

    // Qui NON c'e' piu' una "detection riassunto degenere" che rilegga il TESTO
    // del riassunto (ex `lower.starts_with("[error")`, `contains("billing")`,
    // `contains("nessun provider")`, `len() < 40`). Il fallimento tecnico e' gia'
    // dichiarato in modo STRUTTURATO a monte e filtrato da
    // `esito_tentativo_riassunto`: se il turno arriva fin qui, `error` e'
    // null e il `content` non e' vuoto (regola M). Il blocco lessicale non
    // poteva quindi aggiungere veri positivi, ed era SOLO falso-positivo su un
    // riassunto legittimo — i riassunti di sessioni di debug parlano di errori,
    // di billing e di provider mancanti per mestiere. Quando scattava, la
    // sessione perdeva l'intera memoria E leggeva un 503 che affermava il falso
    // ("tutti i provider AI hanno restituito errore") su una chiamata riuscita.
    // Il ramo `len() < 40` era l'unica differenza non coperta dal segnale
    // strutturato: e' una misura di QUALITA', non un fallimento, e un riassunto
    // breve ma vero vale piu' di nessun riassunto.

    // Sezione "stato lavori" DETERMINISTICA (incidente Beauty-Book): i fatti
    // strutturali dei run (esiti, file toccati) vengono appesi al summary da
    // query, mai delegati alla memoria dell'LLM. Cosi' il prossimo run sa cosa
    // e' gia' stato fatto anche se il riassunto testuale lo omette.
    let summary_text = match structured_work_state(&state.db, session_id).await {
        Some(work_state) => format!("{summary_text}{work_state}"),
        None => summary_text,
    };

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

        let payload = payload_memoria_di_sessione(project_id, session_id, &summary_text);
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
    .execute(&chat_pool)
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
    .execute(&chat_pool)
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
    .execute(&chat_pool)
    .await
    .map_err(|e| CompactError::internal(e.to_string()))?;

    // Mark session as compacted
    sqlx::query("UPDATE chat_sessions SET status = 'compacted', updated_at = NOW() WHERE id = $1")
        .bind(session_id)
        .execute(&chat_pool)
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
    // chat_messages e' migrata: somma sul pool del progetto (chat_pool gia' risolto).
    .fetch_one(&chat_pool)
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

    // Decisioni durature distillate -> worklog (mig 0413), best-effort: il
    // digest provider-neutro le mostra nella sezione "Decisioni:" e sopravvivono
    // alla compattazione successiva (non piu' solo nel testo libero del summary).
    if !distilled_decisions.is_empty() {
        // Worklog nel DB del progetto (separazione DB): riuso il chat_pool gia'
        // risolto in questa funzione.
        let _ = crate::session_worklog::ingest_decisions(
            &state.db,
            &chat_pool,
            session_id,
            Some(project_id),
            &distilled_decisions,
        )
        .await;
    }

    Ok(CompactOutcome {
        summary_text,
        point_id,
        soft_deleted: soft_deleted.rows_affected(),
        total_tokens,
        total_cost_usd,
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

    let ctx = load_session_context(&state, session_id, user_id).await?;

    let outcome = compact_session_core(&state, ctx.session_id, ctx.project_id)
        .await
        .map_err(|e| api_error(e.status, e.message))?;

    Ok(Json(json!({
        "ok": true,
        "summary": outcome.summary_text,
        "pointId": outcome.point_id,
        "totalTokens": outcome.total_tokens,
        "totalCostUsd": outcome.total_cost_usd,
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

    // prompt_corrections e chat_sessions sono entrambe migrate: il JOIN e'
    // valido sul pool del progetto. DB non disponibile -> 503 strutturato.
    let mem_pool = crate::project_db_routes::project_data_pool_from(&state.db, project_id).await?;
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
    .fetch_all(&mem_pool)
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

    // Separazione DB: endpoint keyed solo dalla correzione. prompt_corrections vive
    // nel DB del progetto -> pool via directory di routing (fallback ricerca). La
    // sync Qdrant piu' sotto resta su &state.db (config collection globale).
    let cpool =
        crate::project_db_routes::project_data_pool_by_correction_from(&state.db, memory_id)
            .await?;

    // Flip the active flag
    let new_active: bool = sqlx::query_scalar(
        "UPDATE prompt_corrections SET active = NOT active, updated_at = NOW()
         WHERE id = $1 AND type = 'session_memory'
         RETURNING active",
    )
    .bind(memory_id)
    .fetch_one(&cpool)
    .await
    .map_err(|e| api_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync to Qdrant: retrieve point_id, then update payload
    let qdrant_point_id: String =
        sqlx::query_scalar("SELECT qdrant_point_id FROM prompt_corrections WHERE id = $1")
            .bind(memory_id)
            .fetch_one(&cpool)
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

#[cfg(test)]
mod tests {
    use super::{esito_tentativo_riassunto, normalize_session_pin};
    use crate::internal_routing::AttemptOutcome;
    use crate::nexus_gateway::GwResponse;
    use crate::orchestrator::neural_client::{agent_turn_value_from_gw, error_agent_turn_value};
    use serde_json::json;

    /// Il turno di SUCCESSO come lo costruisce la produzione: la risposta del
    /// gateway arriva deserializzata (`GwResponse`) e passa da
    /// `agent_turn_value_from_gw`. Fabbricare il Value a mano fisserebbe proprio
    /// l'assunto da verificare (regola O).
    fn turno_riuscito(content: &str) -> serde_json::Value {
        let resp: GwResponse = serde_json::from_value(json!({
            "content": content,
            "usage": { "input_tokens": 10, "output_tokens": 20 },
            "model_used": "gemini-2.5-flash",
            "provider_used": "google",
            "latency_ms": 12,
            "finish_reason": "stop",
        }))
        .expect("GwResponse deserializzabile");
        agent_turn_value_from_gw("google", "gemini-2.5-flash", &resp)
    }

    #[test]
    fn un_riassunto_che_parla_di_errori_resta_valido() {
        // Il caso perso dal blocco lessicale rimosso: una sessione di DEBUG
        // produce un riassunto che cita per mestiere errori, billing e provider
        // mancanti. Chi lo ha prodotto ha gia' dichiarato in modo strutturato di
        // NON essere un errore (`error` null, content pieno): il testo non deve
        // avere voce in capitolo.
        let testo = "Sessione di debug: il provider anthropic rispondeva billing_error e \
                     nessun provider del tier era disponibile. Errore del provider AI \
                     risolto ricaricando il credito; controlla i log per i dettagli.";
        let turn = turno_riuscito(testo);
        match esito_tentativo_riassunto(turn) {
            AttemptOutcome::Done(v) => {
                assert_eq!(v.get("content").and_then(|c| c.as_str()), Some(testo));
            }
            AttemptOutcome::Failover => panic!("riassunto legittimo scartato"),
        }
    }

    #[test]
    fn un_riassunto_breve_resta_valido() {
        // Ex `len() < 40`: la brevita' e' una misura di QUALITA', non un
        // fallimento tecnico. Una sessione cortissima ha un riassunto corto.
        let turn = turno_riuscito("Nessuna decisione durevole.");
        assert!(matches!(
            esito_tentativo_riassunto(turn),
            AttemptOutcome::Done(_)
        ));
    }

    #[test]
    fn il_turno_di_errore_e_riconosciuto_dal_segnale_strutturato() {
        // Il turno d'errore come lo costruisce la produzione: content
        // "[Error: ...]" E `error` non-null. E' il caso che il blocco lessicale
        // credeva di coprire da solo; il segnale strutturato lo copre a monte.
        let turn = error_agent_turn_value("anthropic", "claude-x", "billing_error: credito zero");
        assert!(matches!(
            esito_tentativo_riassunto(turn),
            AttemptOutcome::Failover
        ));
    }

    #[test]
    fn il_turno_senza_contenuto_e_un_fallimento() {
        // Ex `summary_text.is_empty()`: coperto dal punto unico (content vuoto
        // = turno improduttivo, es. finish_reason=length su Gemini).
        assert!(matches!(
            esito_tentativo_riassunto(turno_riuscito("   ")),
            AttemptOutcome::Failover
        ));
    }

    #[test]
    fn pin_auto_and_empty_clear_the_preference() {
        // "auto" e stringa vuota (in qualunque forma) azzerano il pin -> NULL:
        // e' la stessa semantica che il frontend invia quando l'utente torna ad
        // "Auto" nel dropdown della chat. Senza questo, un pin "auto" verrebbe
        // salvato come stringa e i run successivi tenterebbero di forzare un
        // provider inesistente.
        assert_eq!(normalize_session_pin("auto"), None);
        assert_eq!(normalize_session_pin("AUTO"), None);
        assert_eq!(normalize_session_pin("  Auto  "), None);
        assert_eq!(normalize_session_pin(""), None);
        assert_eq!(normalize_session_pin("   "), None);
    }

    #[test]
    fn pin_concrete_value_is_trimmed_and_kept() {
        assert_eq!(normalize_session_pin("google"), Some("google".to_string()));
        assert_eq!(
            normalize_session_pin("  anthropic "),
            Some("anthropic".to_string())
        );
        // "automatic" NON e' "auto": e' un valore concreto e va preservato
        // (evita che un match troppo largo lo scarti).
        assert_eq!(
            normalize_session_pin("automatic"),
            Some("automatic".to_string())
        );
    }
}
