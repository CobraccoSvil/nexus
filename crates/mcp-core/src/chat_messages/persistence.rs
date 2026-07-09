use super::*;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatMessageView {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) project_id: String,
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) request_message_id: Option<String>,
    pub(crate) deleted_at: Option<String>,
    pub(crate) created_at: String,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) intent: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) prompt_tokens: Option<i64>,
    /// Prompt token dell'ULTIMA chiamata LLM del run (riempimento contesto).
    /// `prompt_tokens` nel path agentico e' il CUMULATIVO delle iterazioni
    /// (billing): il context ratio della UI deve usare SOLO questo campo.
    /// None per i messaggi persistiti prima della sua introduzione.
    pub(crate) last_prompt_tokens: Option<i64>,
    pub(crate) completion_tokens: Option<i64>,
    pub(crate) total_tokens: Option<i64>,
    pub(crate) total_cost: Option<f64>,
    pub(crate) currency: Option<String>,
    pub(crate) automation_mode: Option<String>,
    pub(crate) resend_of_message_id: Option<String>,
    /// True quando il messaggio e' auto-generato dal sistema (es. auto-continuazione).
    /// La UI nasconde questi messaggi per non confondere l'utente.
    pub(crate) synthetic: bool,
    /// Origine del messaggio quando e' sintetico/di sistema (metadata.source, es.
    /// "process_resume" per i risvegli automatici dell'agente). Segnale STRUTTURATO
    /// (regola M) per la UI: distingue un turno svegliato dal sistema da uno avviato
    /// dall'utente, senza pattern-matching sul testo. None per i messaggi ordinari.
    pub(crate) source: Option<String>,
    /// Sottotipo del messaggio sintetico (metadata.kind), es. l'esito del risveglio
    /// process_resume (success/failed/cap). Con `source` copre il badge senza il
    /// testo. None se assente.
    pub(crate) synthetic_kind: Option<String>,
    /// Stato CANONICO del run che ha prodotto questo messaggio assistant
    /// (agent_runs.status via LEFT JOIN su run_message_id). None per i messaggi
    /// utente o quando il messaggio non e' collegato a un run. Permette alla UI
    /// di mostrare un badge di stato PERSISTENTE (completato/fallito/interrotto/
    /// superato) senza un fetch separato e coerente al reload.
    pub(crate) run_status: Option<String>,
    /// Ragionamento (thinking) del modello accumulato durante il run e persistito
    /// in metadata.reasoning all'INSERT del messaggio assistant (D4). Permette al
    /// refresh di ricostruire il blocco "Ragionamento" identico al live, che lo
    /// alimenta dagli eventi SSE effimeri. None per i messaggi senza reasoning.
    pub(crate) reasoning: Option<String>,
}
pub(crate) fn to_message_view(row: &sqlx::postgres::PgRow) -> Result<ChatMessageView, ApiError> {
    let id: Uuid = row
        .try_get("id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let content: String = row
        .try_get("content")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let request_message_id: Option<Uuid> = row.try_get("request_message_id").unwrap_or(None);
    let deleted_at: Option<DateTime<Utc>> = row.try_get("deleted_at").unwrap_or(None);
    let created_at: DateTime<Utc> = row
        .try_get("created_at")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let metadata: Value = row.try_get("metadata").unwrap_or_else(|_| json!({}));

    Ok(ChatMessageView {
        id: id.to_string(),
        session_id: session_id.to_string(),
        project_id: project_id.to_string(),
        role,
        content,
        request_message_id: request_message_id.map(|value| value.to_string()),
        deleted_at: deleted_at.map(|value| value.to_rfc3339()),
        created_at: created_at.to_rfc3339(),
        provider: metadata
            .get("provider")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        model: metadata
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        intent: metadata
            .get("intent")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        run_id: metadata
            .get("runId")
            .or_else(|| metadata.get("agentRunId"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        prompt_tokens: metadata.get("promptTokens").and_then(Value::as_i64),
        last_prompt_tokens: metadata.get("lastPromptTokens").and_then(Value::as_i64),
        completion_tokens: metadata.get("completionTokens").and_then(Value::as_i64),
        total_tokens: metadata.get("totalTokens").and_then(Value::as_i64),
        total_cost: metadata.get("totalCost").and_then(Value::as_f64),
        currency: metadata
            .get("currency")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        automation_mode: metadata
            .get("automationMode")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        resend_of_message_id: metadata
            .get("resendOf")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        synthetic: metadata
            .get("synthetic")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        source: metadata
            .get("source")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        synthetic_kind: metadata
            .get("kind")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        // Colonna opzionale presente solo nelle query che fanno il LEFT JOIN su
        // agent_runs (es. list_chat_messages). Altrove resta None senza errore.
        run_status: row
            .try_get::<Option<String>, _>("run_status")
            .unwrap_or(None),
        reasoning: metadata
            .get("reasoning")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned),
    })
}
/// Rimuove i NULL byte (\0) dal testo. PostgreSQL jsonb li rifiuta con
/// "unsupported Unicode escape sequence: \0 cannot be converted to text"
/// quando il content del chat_messages viene serializzato in jsonb. Questo e'
/// l'ultimo presidio difensivo: il frontend dovrebbe gia' classificare i file
/// binari come base64 (vedi chat-panel.tsx::handlePickFiles), ma se per
/// qualunque motivo un null byte arriva qui, lo togliamo senza far crashare
/// l'INSERT. Idempotente, no-op per testo gia' pulito.
pub(crate) fn strip_null_bytes(s: &str) -> String {
    if !s.contains('\0') {
        return s.to_string();
    }
    s.replace('\0', "")
}
pub(crate) fn normalize_attachments(input: &[ChatAttachmentRequest]) -> Vec<ChatAttachment> {
    input
        .iter()
        .filter_map(|attachment| {
            let name = attachment.name.trim();
            // Defense-in-depth: rimuovi NULL bytes prima del trim. Vedi
            // `strip_null_bytes` per il motivo (jsonb rifiuta \0).
            let sanitized = strip_null_bytes(&attachment.text_content);
            let text_content = sanitized.trim();
            let has_text = !name.is_empty() && !text_content.is_empty();
            let has_image = !name.is_empty()
                && attachment
                    .base64_content
                    .as_ref()
                    .is_some_and(|b| !b.is_empty());
            if !has_text && !has_image {
                return None;
            }
            Some(ChatAttachment {
                // id propagato successivamente da enrich_attachments_with_ids
                // dopo la persistenza (qui l'UUID non e' ancora disponibile).
                id: None,
                name: name.to_string(),
                mime_type: attachment.mime_type.trim().to_string(),
                size_bytes: attachment.size_bytes.max(0),
                text_content: text_content.to_string(),
                base64_content: attachment.base64_content.clone(),
            })
        })
        .collect()
}
/// Popola `ChatAttachment.id` con gli UUID ottenuti da `persist_message_attachments`.
///
/// Il matching avviene per nome file (case-sensitive, primo match). Mantenere il
/// matching per nome — non per indice — perche' `normalize_attachments` filtra
/// allegati vuoti che potrebbero non avere il corrispondente in `saved`.
///
/// Senza questo passaggio il blocco `<allegati>` nel prompt iniziale mostra solo
/// "- file.ext (mime, N byte)" senza `[ID: <uuid>]`, costringendo il modello a
/// guessare l'attachment_id quando chiama `nexus_inspect_attachment` (osservato
/// 30/05/2026: Vertex passa il filename come id e il tool ritorna "Allegato
/// {filename} non trovato" — G1 cap chiude il turn).
pub(crate) fn enrich_attachments_with_ids(
    mut atts: Vec<ChatAttachment>,
    saved: &[crate::chat_attachments::SavedAttachment],
) -> Vec<ChatAttachment> {
    for att in atts.iter_mut() {
        if att.id.is_some() {
            continue;
        }
        if let Some(s) = saved.iter().find(|s| s.file_name == att.name) {
            att.id = Uuid::parse_str(&s.id).ok();
        }
    }
    atts
}
/// Variante di `enrich_attachments_with_ids` che fetcha gli ID dal DB per gli
/// allegati gia' persistiti in `chat_message_attachments`. Usato dai code path
/// di resend/regenerate dove la persistenza e' avvenuta in un turno precedente
/// e non e' disponibile il `Vec<SavedAttachment>`.
///
/// Separazione DB: `chat_message_attachments` e' migrata nel DB del progetto. Il
/// chiamante DEVE passare un pool GIA' instradato al progetto (risolto via
/// `project_data_pool_by_message_from` o affine) — NON il meta-DB, che a flag ON
/// non contiene piu' queste righe e ritornerebbe vuoto.
pub(crate) async fn enrich_attachments_with_ids_from_db(
    pool: &PgPool,
    atts: Vec<ChatAttachment>,
    message_id: Uuid,
) -> Vec<ChatAttachment> {
    if atts.is_empty() {
        return atts;
    }
    let rows = match sqlx::query(
        r#"SELECT id, file_name FROM chat_message_attachments
           WHERE message_id = $1 ORDER BY created_at ASC"#,
    )
    .bind(message_id)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                message_id = %message_id,
                error = %e,
                "enrich_attachments_with_ids_from_db: query fallita"
            );
            return atts;
        }
    };
    let mut by_name: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    for row in rows.iter() {
        let id: Uuid = match row.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let name: String = row.try_get("file_name").unwrap_or_default();
        by_name.entry(name).or_insert(id);
    }
    atts.into_iter()
        .map(|mut a| {
            if a.id.is_none() {
                if let Some(id) = by_name.get(&a.name) {
                    a.id = Some(*id);
                }
            }
            a
        })
        .collect()
}
/// Esito dell'insert con chiave di idempotenza (`client_message_id`).
#[derive(Debug)]
pub(crate) enum ClientIdInsert {
    Inserted(Uuid),
    /// Unique violation su (session_id, client_message_id) — mig progetto 0008:
    /// la stessa POST e' gia' stata persistita da un tentativo concorrente.
    /// Il chiamante deve fare replay della risposta, mai duplicare.
    Duplicate,
}

pub(crate) async fn insert_message(
    db: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    role: &str,
    content: &str,
    metadata: Value,
    request_message_id: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    match insert_message_with_client_id(
        db,
        session_id,
        project_id,
        role,
        content,
        metadata,
        request_message_id,
        None,
    )
    .await?
    {
        ClientIdInsert::Inserted(id) => Ok(id),
        // Impossibile senza client_message_id (l'indice unico e' parziale su
        // client_message_id IS NOT NULL); mappato a errore esplicito per non
        // nascondere un eventuale bug di schema.
        ClientIdInsert::Duplicate => Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "unique violation inattesa su insert_message senza client_message_id",
        )),
    }
}

/// Punto unico (regola L) dell'INSERT in chat_messages. Con `client_message_id`
/// valorizzato l'insert e' idempotente: una unique violation sull'indice
/// parziale (session_id, client_message_id) NON e' un errore ma il segnale
/// strutturato (codice SQLSTATE 23505, regola M) che un retry della stessa POST
/// e' gia' andato a buon fine.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_message_with_client_id(
    db: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    role: &str,
    content: &str,
    metadata: Value,
    request_message_id: Option<Uuid>,
    client_message_id: Option<Uuid>,
) -> Result<ClientIdInsert, ApiError> {
    let message_id = Uuid::new_v4();
    // Cutover separazione DB (route-at-helper): il messaggio si scrive nel DB del
    // progetto via registry globale. `db` e' il pool meta-DB per la risoluzione;
    // flag off -> ritorna il meta-DB (comportamento storico).
    let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let insert_result = sqlx::query(
        r#"
        INSERT INTO chat_messages (
            id, session_id, project_id, role, content, metadata, request_message_id,
            client_message_id, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW())
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .bind(project_id)
    .bind(role)
    .bind(content)
    .bind(metadata)
    .bind(request_message_id)
    .bind(client_message_id)
    .execute(&pool)
    .await;

    if let Err(e) = insert_result {
        // Segnale strutturato, mai parsing del messaggio (regola M): SQLSTATE
        // 23505 = unique_violation. Rilevante solo quando la chiave di
        // idempotenza e' presente (l'indice e' parziale).
        let is_unique_violation = matches!(
            &e,
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23505")
        );
        if is_unique_violation && client_message_id.is_some() {
            return Ok(ClientIdInsert::Duplicate);
        }
        return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    // Directory di routing (meta): registra message_id -> project_id cosi' gli
    // endpoint keyed solo dal messaggio (feedback, delete) risolvono il pool.
    crate::project_db_routes::register_entity_routing(db, "message", message_id, project_id).await;

    Ok(ClientIdInsert::Inserted(message_id))
}
/// Allegati di UN messaggio in forma JSON (stesso shape di `savedAttachments`
/// nel path di invio normale). Punto unico (regola L) riusato dal replay
/// idempotente dell'invio: senza, i chip e la proposta di indicizzazione KB si
/// perderebbero quando il client ritenta una POST gia' persistita.
pub(crate) async fn message_attachments_json(pool: &PgPool, message_id: Uuid) -> Vec<Value> {
    sqlx::query(
        r#"
        SELECT id, project_id, file_name, file_path, mime_type,
               size_bytes, kind, kb_note_id, indexed_at, created_at
        FROM chat_message_attachments
        WHERE message_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(message_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .iter()
    .filter_map(|row| {
        let att_id: Uuid = row.try_get("id").ok()?;
        let project_id: Uuid = row.try_get("project_id").ok()?;
        let kb_note_id: Option<Uuid> = row.try_get("kb_note_id").unwrap_or(None);
        let indexed_at: Option<DateTime<Utc>> = row.try_get("indexed_at").unwrap_or(None);
        let created_at: Option<DateTime<Utc>> = row.try_get("created_at").ok();
        Some(json!({
            "id": att_id.to_string(),
            "messageId": message_id.to_string(),
            "projectId": project_id.to_string(),
            "fileName": row.try_get::<String, _>("file_name").unwrap_or_default(),
            "filePath": row.try_get::<String, _>("file_path").unwrap_or_default(),
            "mimeType": row.try_get::<String, _>("mime_type").unwrap_or_default(),
            "sizeBytes": row.try_get::<i64, _>("size_bytes").unwrap_or(0),
            "kind": row.try_get::<String, _>("kind").unwrap_or_else(|_| "binary".to_string()),
            "kbNoteId": kb_note_id.map(|v| v.to_string()),
            "indexedAt": indexed_at.map(|v| v.to_rfc3339()),
            "createdAt": created_at.map(|v| v.to_rfc3339()),
        }))
    })
    .collect()
}

pub(crate) async fn load_message_by_id(
    db: &PgPool,
    project_id: Uuid,
    message_id: Uuid,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    // Legge dal DB del progetto dove il messaggio e' stato scritto (route-at-helper).
    let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.content,
            m.metadata,
            m.request_message_id,
            m.deleted_at,
            m.created_at
        FROM chat_messages m
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"))
}
