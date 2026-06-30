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
    pub(crate) completion_tokens: Option<i64>,
    pub(crate) total_tokens: Option<i64>,
    pub(crate) total_cost: Option<f64>,
    pub(crate) currency: Option<String>,
    pub(crate) automation_mode: Option<String>,
    pub(crate) resend_of_message_id: Option<String>,
    /// True quando il messaggio e' auto-generato dal sistema (es. auto-continuazione).
    /// La UI nasconde questi messaggi per non confondere l'utente.
    pub(crate) synthetic: bool,
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
        // Colonna opzionale presente solo nelle query che fanno il LEFT JOIN su
        // agent_runs (es. list_chat_messages). Altrove resta None senza errore.
        run_status: row.try_get::<Option<String>, _>("run_status").unwrap_or(None),
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
pub(crate) async fn enrich_attachments_with_ids_from_db(
    db: &PgPool,
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
    .fetch_all(db)
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
pub(crate) async fn insert_message(
    db: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    role: &str,
    content: &str,
    metadata: Value,
    request_message_id: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    let message_id = Uuid::new_v4();
    // Cutover separazione DB (route-at-helper): il messaggio si scrive nel DB del
    // progetto via registry globale. `db` e' il pool meta-DB per la risoluzione;
    // flag off -> ritorna il meta-DB (comportamento storico).
    let pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    sqlx::query(
        r#"
        INSERT INTO chat_messages (
            id, session_id, project_id, role, content, metadata, request_message_id, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .bind(project_id)
    .bind(role)
    .bind(content)
    .bind(metadata)
    .bind(request_message_id)
    .execute(&pool)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(message_id)
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
