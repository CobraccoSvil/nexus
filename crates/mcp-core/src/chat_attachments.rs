//! Persistenza degli allegati alla chat e indicizzazione opzionale nella KB.
//!
//! Quando l'utente allega file (testo trascinato o immagine incollata) al
//! chat-panel, il flusso e':
//!   1. `send_chat_message` salva la riga `chat_messages` user
//!   2. `persist_message_attachments` scrive i bytes su disco in
//!      `<project_root>/.nexus/attachments/<msg_id>/<file_safe>` e inserisce
//!      una riga in `chat_message_attachments`
//!   3. Il frontend mostra i chip e (opzionalmente) chiede all'utente se
//!      vuole indicizzare gli allegati nella Knowledge Base
//!   4. Su conferma chiama POST `/api/chat/messages/:id/attachments/index`
//!      che genera una nota in `project_knowledge_notes` con embedding in
//!      Qdrant (collezione `knowledge_notes`), riusando la pipeline KB.
//!
//! La cancellazione e' a cascata: se l'utente cancella il messaggio o il
//! progetto, le righe in `chat_message_attachments` vengono rimosse via
//! `ON DELETE CASCADE`. La nota KB resta (ma il riferimento `kb_note_id`
//! viene messo a NULL).
//!
//! Niente leak in log: i contenuti raw dei file non vengono mai stampati,
//! solo nome/dimensione/mime.

use std::path::{Component, Path, PathBuf};

use axum::{
    body::Body,
    extract::{Extension, Path as AxumPath, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{api_error, ensure_project_access, parse_user_id, ApiError, ApiResult},
    chat_messages::ChatAttachmentRequest,
    knowledge::{extract_tags, title_from_content},
    AppState,
};

/// Numero massimo di byte del file letti come excerpt da indicizzare nella KB.
/// Excerpt piu' grossi saturano il limite embedding (~2KB di testo) e i token
/// di contesto, quindi tagliamo qui prima di passare al pipeline KB.
const KB_EXCERPT_MAX_BYTES: usize = 16 * 1024;

/// Lunghezza massima del nome file salvato su disco (dopo sanitizzazione).
const SANITIZED_FILENAME_MAX_LEN: usize = 120;

/// Mime type considerati "text" anche se non iniziano con `text/`.
/// L'elenco viene mantenuto stretto: tipi noti, indicizzabili come testo
/// nel KB. Tutto il resto rientra in `binary` per default.
const TEXTUAL_MIME_WHITELIST: &[&str] = &[
    "application/json",
    "application/xml",
    "application/yaml",
    "application/x-yaml",
    "application/javascript",
    "application/typescript",
    "application/sql",
    "application/x-sh",
    "application/x-shellscript",
    "application/toml",
    "application/x-toml",
    "application/markdown",
    "application/x-markdown",
    "application/csv",
];

/// Categoria dell'allegato derivata dal mime type. Determina la pipeline
/// (indicizzazione KB testuale vs. metadata-only per immagini).
pub fn classify_attachment_kind(mime_type: &str) -> &'static str {
    let lowered = mime_type.trim().to_lowercase();
    if lowered.starts_with("image/") {
        "image"
    } else if lowered.starts_with("text/") || TEXTUAL_MIME_WHITELIST.contains(&lowered.as_str()) {
        "text"
    } else {
        "binary"
    }
}

/// Restituisce un nome file sicuro per il filesystem: rimuove path traversal,
/// rimpiazza caratteri non sicuri con `_`, tronca a `SANITIZED_FILENAME_MAX_LEN`.
/// Se l'input e' vuoto o composto solo da caratteri non sicuri, ritorna un
/// fallback deterministico basato su un suffisso "file".
pub fn sanitize_attachment_filename(name: &str) -> String {
    // Prendi solo l'ultimo componente: scarta eventuali separatori inviati dal client.
    let trimmed = name.trim();
    let last_component = trimmed
        .rsplit(|c: char| c == '/' || c == '\\')
        .next()
        .unwrap_or("")
        .trim();

    let mut out = String::with_capacity(last_component.len());
    let mut last_was_underscore = false;
    for ch in last_component.chars() {
        let is_safe = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        if is_safe {
            out.push(ch);
            last_was_underscore = false;
        } else if !last_was_underscore {
            out.push('_');
            last_was_underscore = true;
        }
    }

    // Niente dot-leading (impedisce file nascosti) e niente "..".
    while out.starts_with('.') {
        out.remove(0);
    }
    if out == ".." || out.contains("..") {
        out = out.replace("..", "_");
    }

    if out.is_empty() {
        out = format!("file-{}", &Uuid::new_v4().to_string()[..8]);
    }

    if out.len() > SANITIZED_FILENAME_MAX_LEN {
        // Mantieni l'estensione se presente.
        let (stem, ext) = match out.rsplit_once('.') {
            Some((s, e)) if !e.is_empty() && e.len() < 16 => (s.to_string(), format!(".{e}")),
            _ => (out.clone(), String::new()),
        };
        let keep = SANITIZED_FILENAME_MAX_LEN.saturating_sub(ext.len());
        out = format!("{}{ext}", &stem[..stem.len().min(keep)]);
    }

    out
}

/// Verifica che `candidate` non esca dalla directory `base` (no `..`, no link
/// assoluti). Ritorna errore se la canonicalizzazione mostra un parent escape.
fn ensure_path_within(base: &Path, candidate: &Path) -> Result<(), ApiError> {
    // Refuse percorsi assoluti o componenti "parent" residui post-sanitize.
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                return Err(api_error(
                    StatusCode::BAD_REQUEST,
                    "Percorso allegato non sicuro: contiene componenti '..'",
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                if !candidate.starts_with(base) {
                    return Err(api_error(
                        StatusCode::BAD_REQUEST,
                        "Percorso allegato non sicuro: root assoluta",
                    ));
                }
            }
            _ => {}
        }
    }
    if !candidate.starts_with(base) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Percorso allegato fuori dalla directory consentita",
        ));
    }
    Ok(())
}

/// Rappresentazione di un allegato salvato — restituita all'API per il
/// frontend dopo l'invio del messaggio.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAttachment {
    pub id: String,
    pub message_id: String,
    pub project_id: String,
    pub file_name: String,
    pub file_path: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub kind: String,
    pub kb_note_id: Option<String>,
    pub indexed_at: Option<String>,
    pub created_at: String,
}

/// Salva sul filesystem e su DB gli allegati passati. Eseguito sincronamente
/// dentro `send_chat_message` per garantire che il response contenga gli ID.
/// Errori filesystem singoli non interrompono il batch: vengono loggati e
/// l'allegato viene saltato (best-effort).
pub async fn persist_message_attachments(
    db: &PgPool,
    project_root: &Path,
    project_id: Uuid,
    message_id: Uuid,
    attachments: &[ChatAttachmentRequest],
) -> Vec<SavedAttachment> {
    if attachments.is_empty() {
        return Vec::new();
    }

    let base_dir = project_root
        .join(".nexus")
        .join("attachments")
        .join(message_id.to_string());

    if let Err(e) = tokio::fs::create_dir_all(&base_dir).await {
        tracing::warn!(
            project_id = %project_id,
            message_id = %message_id,
            error = %e,
            "creazione directory allegati fallita"
        );
        return Vec::new();
    }

    let mut saved = Vec::with_capacity(attachments.len());

    for attachment in attachments {
        let original_name = attachment.name.trim();
        if original_name.is_empty() {
            tracing::debug!(message_id = %message_id, "allegato senza nome: skip");
            continue;
        }

        let mime_type = attachment.mime_type.trim().to_string();
        let kind = classify_attachment_kind(&mime_type).to_string();
        let safe_name = sanitize_attachment_filename(original_name);
        let final_path = base_dir.join(&safe_name);

        if let Err(e) = ensure_path_within(&base_dir, &final_path) {
            tracing::warn!(
                message_id = %message_id,
                file = %original_name,
                "path allegato non sicuro: {}",
                e.1["error"].as_str().unwrap_or("errore percorso")
            );
            continue;
        }

        // Risolvi i bytes: priorita' base64 (immagini/binari) > text_content.
        let bytes_result: Result<Vec<u8>, String> = if let Some(b64) = &attachment.base64_content {
            if b64.is_empty() {
                if attachment.text_content.is_empty() {
                    Err("contenuto vuoto".into())
                } else {
                    Ok(attachment.text_content.as_bytes().to_vec())
                }
            } else {
                BASE64_STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| format!("base64 invalido: {e}"))
            }
        } else if !attachment.text_content.is_empty() {
            Ok(attachment.text_content.as_bytes().to_vec())
        } else {
            Err("contenuto vuoto".into())
        };

        let bytes = match bytes_result {
            Ok(b) => b,
            Err(reason) => {
                tracing::warn!(
                    message_id = %message_id,
                    file = %safe_name,
                    "allegato saltato: {reason}"
                );
                continue;
            }
        };

        if let Err(e) = tokio::fs::write(&final_path, &bytes).await {
            tracing::warn!(
                message_id = %message_id,
                file = %safe_name,
                error = %e,
                "scrittura allegato su disco fallita"
            );
            continue;
        }

        let actual_size = bytes.len() as i64;
        let path_string = final_path.to_string_lossy().to_string();
        let attachment_id = Uuid::new_v4();
        let created_at = Utc::now();

        let insert = sqlx::query(
            r#"
            INSERT INTO chat_message_attachments (
                id, message_id, project_id, file_name, file_path,
                mime_type, size_bytes, kind, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(attachment_id)
        .bind(message_id)
        .bind(project_id)
        .bind(&safe_name)
        .bind(&path_string)
        .bind(&mime_type)
        .bind(actual_size)
        .bind(&kind)
        .bind(created_at)
        .execute(db)
        .await;

        if let Err(e) = insert {
            tracing::warn!(
                message_id = %message_id,
                file = %safe_name,
                error = %e,
                "insert chat_message_attachments fallito"
            );
            // Pulisci il file che abbiamo gia' scritto per non lasciare orfani.
            let _ = tokio::fs::remove_file(&final_path).await;
            continue;
        }

        // Cloni per spawn RAG (sopravvivono al move dentro SavedAttachment).
        let mime_type_for_rag = mime_type.clone();
        let original_name_for_rag = safe_name.clone();
        saved.push(SavedAttachment {
            id: attachment_id.to_string(),
            message_id: message_id.to_string(),
            project_id: project_id.to_string(),
            file_name: safe_name,
            file_path: path_string,
            mime_type,
            size_bytes: actual_size,
            kind,
            kb_note_id: None,
            indexed_at: None,
            created_at: created_at.to_rfc3339(),
        });

        // RAG (ADR 0015): indicizzazione fire-and-forget dell'allegato
        // appena persistito. Non blocca la response. Il fallimento e' loggato.
        {
            let db_clone = db.clone();
            let file_path_clone = final_path.clone();
            let mime_clone = mime_type_for_rag.clone();
            let name_clone = original_name_for_rag.clone();
            let pid = project_id;
            tokio::spawn(async move {
                if let Err(e) = crate::rag::index_attachment(
                    &db_clone,
                    attachment_id,
                    file_path_clone,
                    mime_clone,
                    name_clone,
                    Some(pid),
                    None,
                )
                .await
                {
                    tracing::warn!(
                        "rag: indicizzazione allegato {} fallita: {}",
                        attachment_id,
                        e
                    );
                }
            });
        }

    }

    saved
}

/// Lista degli allegati associati a un messaggio (usata per popolare la UI
/// quando si ricarica una sessione).
pub async fn list_attachments_for_message(
    db: &PgPool,
    message_id: Uuid,
) -> Result<Vec<SavedAttachment>, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, message_id, project_id, file_name, file_path,
               mime_type, size_bytes, kind, kb_note_id, indexed_at, created_at
        FROM chat_message_attachments
        WHERE message_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(message_id)
    .fetch_all(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row
            .try_get("id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let msg_id: Uuid = row
            .try_get("message_id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let project_id: Uuid = row
            .try_get("project_id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let file_name: String = row
            .try_get("file_name")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let file_path: String = row
            .try_get("file_path")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let mime_type: String = row
            .try_get("mime_type")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let size_bytes: i64 = row
            .try_get("size_bytes")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let kind: String = row
            .try_get("kind")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let kb_note_id: Option<Uuid> = row.try_get("kb_note_id").unwrap_or(None);
        let indexed_at: Option<DateTime<Utc>> = row.try_get("indexed_at").unwrap_or(None);
        let created_at: DateTime<Utc> = row
            .try_get("created_at")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        out.push(SavedAttachment {
            id: id.to_string(),
            message_id: msg_id.to_string(),
            project_id: project_id.to_string(),
            file_name,
            file_path,
            mime_type,
            size_bytes,
            kind,
            kb_note_id: kb_note_id.map(|v| v.to_string()),
            indexed_at: indexed_at.map(|v| v.to_rfc3339()),
            created_at: created_at.to_rfc3339(),
        });
    }
    Ok(out)
}

/// Serializza una lista di SavedAttachment in JSON pronto per la risposta API.
pub fn attachments_to_json(items: &[SavedAttachment]) -> Value {
    serde_json::to_value(items).unwrap_or_else(|_| json!([]))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexAttachmentsRequest {
    /// Lista degli ID degli allegati da indicizzare. Vuota = nessuna azione.
    pub attachment_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexedAttachment {
    attachment_id: String,
    kb_note_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkippedAttachment {
    attachment_id: String,
    reason: String,
}

#[derive(Debug)]
struct AttachmentRecord {
    id: Uuid,
    project_id: Uuid,
    message_id: Uuid,
    file_name: String,
    file_path: String,
    mime_type: String,
    size_bytes: i64,
    kind: String,
    kb_note_id: Option<Uuid>,
}

async fn load_attachment_record(
    db: &PgPool,
    attachment_id: Uuid,
) -> Result<Option<AttachmentRecord>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT id, project_id, message_id, file_name, file_path,
               mime_type, size_bytes, kind, kb_note_id
        FROM chat_message_attachments
        WHERE id = $1
        "#,
    )
    .bind(attachment_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else { return Ok(None) };

    Ok(Some(AttachmentRecord {
        id: row
            .try_get("id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        project_id: row
            .try_get("project_id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        message_id: row
            .try_get("message_id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        file_name: row
            .try_get("file_name")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        file_path: row
            .try_get("file_path")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        mime_type: row
            .try_get("mime_type")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        size_bytes: row
            .try_get("size_bytes")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        kind: row
            .try_get("kind")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        kb_note_id: row.try_get("kb_note_id").unwrap_or(None),
    }))
}

/// POST `/api/chat/messages/:message_id/attachments/index`
///
/// Per ogni `attachmentIds[i]`:
///   - se gia' indicizzato (`kb_note_id` non NULL) -> skip "gia' indicizzato"
///   - se `kind = 'binary'` -> skip "tipo non indicizzabile"
///   - se `kind = 'text'`  -> legge il file (max 16KB) e crea nota in
///     `project_knowledge_notes` con embedding via `neural.embed_text` e
///     upsert in Qdrant tramite `vector_memory::upsert_knowledge_point`
///   - se `kind = 'image'` -> crea nota "metadata-only" (titolo+tags) senza
///     contenuto raw. OCR e multimodal sono fuori scope V1.
///
/// Aggiorna poi `chat_message_attachments.kb_note_id` e `indexed_at`.
pub async fn index_attachments_to_kb(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
    Json(body): Json<IndexAttachmentsRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    if body.attachment_ids.is_empty() {
        return Ok(Json(json!({
            "indexed": [],
            "skipped": [],
        })));
    }

    let mut indexed: Vec<IndexedAttachment> = Vec::new();
    let mut skipped: Vec<SkippedAttachment> = Vec::new();

    for raw_id in &body.attachment_ids {
        let attachment_id = match Uuid::parse_str(raw_id) {
            Ok(id) => id,
            Err(_) => {
                skipped.push(SkippedAttachment {
                    attachment_id: raw_id.clone(),
                    reason: "ID non valido".into(),
                });
                continue;
            }
        };

        let record = match load_attachment_record(&state.db, attachment_id).await? {
            Some(r) => r,
            None => {
                skipped.push(SkippedAttachment {
                    attachment_id: raw_id.clone(),
                    reason: "Allegato non trovato".into(),
                });
                continue;
            }
        };

        if record.message_id != message_id {
            skipped.push(SkippedAttachment {
                attachment_id: raw_id.clone(),
                reason: "Allegato non appartiene al messaggio indicato".into(),
            });
            continue;
        }

        // Access check: l'utente deve avere accesso al progetto dell'allegato.
        ensure_project_access(&state.db, user_id, record.project_id).await?;

        if record.kb_note_id.is_some() {
            skipped.push(SkippedAttachment {
                attachment_id: raw_id.clone(),
                reason: "Allegato gia' indicizzato".into(),
            });
            continue;
        }

        if record.kind == "binary" {
            skipped.push(SkippedAttachment {
                attachment_id: raw_id.clone(),
                reason: "Formato binario non indicizzabile in KB".into(),
            });
            continue;
        }

        // Per text leggiamo il file da disco; per image creiamo nota metadata-only.
        let body_md = match record.kind.as_str() {
            "text" => {
                match tokio::fs::read(&record.file_path).await {
                    Ok(bytes) => {
                        let truncated = if bytes.len() > KB_EXCERPT_MAX_BYTES {
                            &bytes[..KB_EXCERPT_MAX_BYTES]
                        } else {
                            &bytes[..]
                        };
                        let mut excerpt = String::from_utf8_lossy(truncated).to_string();
                        if bytes.len() > KB_EXCERPT_MAX_BYTES {
                            excerpt.push_str("\n\n... [troncato a 16 KB per indicizzazione KB]");
                        }
                        format!(
                            "Allegato chat: {}\nMime: {}\nDimensione: {} byte\n\n```\n{}\n```",
                            record.file_name, record.mime_type, record.size_bytes, excerpt
                        )
                    }
                    Err(e) => {
                        skipped.push(SkippedAttachment {
                            attachment_id: raw_id.clone(),
                            reason: format!("Lettura file fallita: {e}"),
                        });
                        continue;
                    }
                }
            }
            "image" => format!(
                "Allegato immagine: {}\nMime: {}\nDimensione: {} byte\n\nNota metadata-only: \
                il contenuto binario dell'immagine non viene indicizzato in V1. \
                Si rimanda al file su disco: {}",
                record.file_name, record.mime_type, record.size_bytes, record.file_path
            ),
            other => {
                skipped.push(SkippedAttachment {
                    attachment_id: raw_id.clone(),
                    reason: format!("Tipo allegato sconosciuto: {other}"),
                });
                continue;
            }
        };

        // Genera embedding + upsert Qdrant — stesso pattern di create_note_manual.
        let note_id = Uuid::new_v4();
        let title = title_from_content(&record.file_name, 100);
        let mut tags = extract_tags(&body_md);
        tags.push("attachment".to_string());
        tags.push(record.mime_type.clone());
        // Dedup (extract_tags potrebbe restituire una collezione gia' unica
        // ma siamo cauti dopo i push manuali).
        tags.sort();
        tags.dedup();

        let intent = "attachment";

        let embed_text = if body_md.len() > 2000 {
            &body_md[..2000]
        } else {
            body_md.as_str()
        };

        let qdrant_point_id = match state.orchestrator.neural.embed_text("", embed_text).await {
            Ok(vector) => {
                let point_id = Uuid::new_v4().to_string();
                let payload = json!({
                    "project_id": record.project_id.to_string(),
                    "note_id": note_id.to_string(),
                    "intent": intent,
                    "status": "active",
                });
                match crate::vector_memory::upsert_knowledge_point(
                    &state.db,
                    &point_id,
                    vector,
                    payload,
                )
                .await
                {
                    Ok(_) => Some(point_id),
                    Err(e) => {
                        tracing::warn!(
                            attachment_id = %record.id,
                            error = %e,
                            "upsert Qdrant fallito per allegato (proseguo con nota DB)"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    attachment_id = %record.id,
                    error = %e,
                    "embed_text fallito per allegato (nota senza embedding)"
                );
                None
            }
        };

        // INSERT nota.
        let insert_res = sqlx::query(
            r#"
            INSERT INTO project_knowledge_notes
                (id, project_id, intent, title, body_md, status,
                 qdrant_point_id, tags, file_paths, source_message_id, kind)
            VALUES ($1, $2, $3, $4, $5, 'active', $6, $7, $8, $9, 'manual')
            "#,
        )
        .bind(note_id)
        .bind(record.project_id)
        .bind(intent)
        .bind(&title)
        .bind(&body_md)
        .bind(qdrant_point_id.as_deref())
        .bind(&tags)
        .bind(vec![record.file_path.clone()])
        .bind(record.message_id)
        .execute(&state.db)
        .await;

        if let Err(e) = insert_res {
            tracing::warn!(
                attachment_id = %record.id,
                error = %e,
                "insert nota KB fallito"
            );
            skipped.push(SkippedAttachment {
                attachment_id: raw_id.clone(),
                reason: "Creazione nota KB fallita".into(),
            });
            continue;
        }

        // Aggiorna tags aggregati (best-effort, non fatale).
        for tag in &tags {
            let _ = sqlx::query(
                r#"
                INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
                VALUES ($1, $2, 1, NOW())
                ON CONFLICT (project_id, tag) DO UPDATE
                SET note_count = project_knowledge_tags.note_count + 1,
                    last_used_at = NOW()
                "#,
            )
            .bind(record.project_id)
            .bind(tag)
            .execute(&state.db)
            .await;
        }

        // Aggiorna allegato con kb_note_id + indexed_at.
        let _ = sqlx::query(
            r#"
            UPDATE chat_message_attachments
            SET kb_note_id = $1, indexed_at = NOW()
            WHERE id = $2
            "#,
        )
        .bind(note_id)
        .bind(record.id)
        .execute(&state.db)
        .await;

        // Emit SSE per aggiornare il pannello KB.
        let _ = nexus_events::dispatcher::emit(
            &state.project_channels,
            record.project_id,
            nexus_events::ProjectEvent::KnowledgeNoteCreated {
                note_id,
                title: title.clone(),
                intent: Some(intent.to_string()),
            },
        );

        indexed.push(IndexedAttachment {
            attachment_id: raw_id.clone(),
            kb_note_id: note_id.to_string(),
        });
    }

    Ok(Json(json!({
        "indexed": indexed,
        "skipped": skipped,
    })))
}

/// GET `/api/chat/attachments/:attachment_id/raw`
///
/// Restituisce i bytes raw del file allegato con il mime_type corretto.
/// Usato dal frontend per:
///   - thumbnail immagini (`<img src="/api/chat/attachments/{id}/raw" />`)
///   - download generico (link cliccabile con `download` attribute)
///
/// Sicurezza: verifica che l'utente abbia accesso al progetto a cui appartiene
/// l'allegato. Il path su disco e' quello memorizzato in DB (gia' sanitizzato
/// a save-time) e viene letto solo se la query DB ha avuto successo.
pub async fn get_attachment_raw(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(attachment_id): AxumPath<String>,
) -> Result<Response<Body>, ApiError> {
    let user_id = parse_user_id(&claims)?;
    let attachment_id = Uuid::parse_str(&attachment_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Attachment id non valido"))?;

    let record = match load_attachment_record(&state.db, attachment_id).await? {
        Some(r) => r,
        None => return Err(api_error(StatusCode::NOT_FOUND, "Allegato non trovato")),
    };

    ensure_project_access(&state.db, user_id, record.project_id).await?;

    let bytes = tokio::fs::read(&record.file_path).await.map_err(|e| {
        tracing::warn!(
            attachment_id = %record.id,
            file = %record.file_path,
            error = %e,
            "lettura allegato fallita"
        );
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Impossibile leggere l'allegato",
        )
    })?;

    let content_type = HeaderValue::from_str(&record.mime_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));

    // Content-Disposition: inline per immagini (renderizzate dal browser),
    // attachment per il resto (forza il download con il nome originale).
    let disposition = if record.kind == "image" {
        format!("inline; filename=\"{}\"", sanitize_attachment_filename(&record.file_name))
    } else {
        format!("attachment; filename=\"{}\"", sanitize_attachment_filename(&record.file_name))
    };

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&disposition).unwrap_or_else(|_| HeaderValue::from_static("inline")),
        )
        // Cache moderata: i file sono immutabili una volta scritti, ma l'utente
        // potrebbe cancellare il messaggio (ON DELETE CASCADE) -> 5 min basta.
        .header(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"))
        .body(Body::from(bytes))
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(response.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_attachment_filename("foo.txt"), "foo.txt");
        assert_eq!(sanitize_attachment_filename(" foo bar.txt "), "foo_bar.txt");
    }

    #[test]
    fn sanitize_removes_path_traversal() {
        assert_eq!(sanitize_attachment_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_attachment_filename("..\\foo.txt"), "foo.txt");
        let s = sanitize_attachment_filename("..");
        assert!(!s.contains(".."), "got: {s}");
    }

    #[test]
    fn sanitize_strips_dot_prefix() {
        assert_eq!(sanitize_attachment_filename(".env"), "env");
        assert_eq!(sanitize_attachment_filename("..bashrc"), "bashrc");
    }

    #[test]
    fn sanitize_empty_or_only_unsafe() {
        let s = sanitize_attachment_filename("");
        assert!(s.starts_with("file-"));
        let s2 = sanitize_attachment_filename("///");
        assert!(s2.starts_with("file-"));
    }

    #[test]
    fn classify_known_mimes() {
        assert_eq!(classify_attachment_kind("image/png"), "image");
        assert_eq!(classify_attachment_kind("image/JPEG"), "image");
        assert_eq!(classify_attachment_kind("text/plain"), "text");
        assert_eq!(classify_attachment_kind("application/json"), "text");
        assert_eq!(classify_attachment_kind("application/octet-stream"), "binary");
        assert_eq!(classify_attachment_kind(""), "binary");
    }

    #[test]
    fn ensure_within_rejects_parent() {
        let base = PathBuf::from("/srv/nexus/.nexus/attachments/abc");
        let evil = base.join("../../../etc/passwd");
        assert!(ensure_path_within(&base, &evil).is_err());
        let safe = base.join("file.txt");
        assert!(ensure_path_within(&base, &safe).is_ok());
    }
}
