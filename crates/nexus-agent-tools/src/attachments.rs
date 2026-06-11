//! Tool nexus_list_attachments / nexus_read_attachment.
//!
//! Permettono all'agente di scoprire gli allegati caricati dall'utente nel
//! turno corrente (o in qualsiasi turno della sessione) e di leggerli a
//! richiesta, in modalita' streaming-friendly (offset+length).
//!
//! Vedi ADR 0010 e migrazione 0192.

use std::io::SeekFrom;

use base64::Engine;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

use super::read_cache::{self, ReadCacheKey, ReadKind};
use super::ToolContextCore;

/// Max bytes leggibili in una singola chiamata di nexus_read_attachment.
const MAX_READ_BYTES: usize = 102_400; // 100 KB

/// MIME considerati testuali nonostante non inizino con "text/".
const TEXT_LIKE_MIMES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/x-sh",
    "application/x-makefile",
    "application/javascript",
    "application/yaml",
    "application/toml",
    "application/x-yaml",
];

fn err_json(msg: impl Into<String>) -> String {
    json!({ "error": msg.into() }).to_string()
}

/// Lista gli allegati di una sessione chat.
///
/// Input: { "session_id": <uuid?> } — opzionale, default = ctx.session_id.
pub async fn tool_nexus_list_attachments(ctx: &ToolContextCore, input: &Value) -> String {
    let session_id: Uuid = match input.get("session_id").and_then(Value::as_str) {
        Some(s) => match Uuid::parse_str(s) {
            Ok(u) => u,
            Err(_) => return err_json("Parametro 'session_id' non e' un UUID valido"),
        },
        None => match ctx.session_id {
            Some(s) => s,
            None => {
                return err_json(
                    "Nessuna session_id disponibile nel contesto. Passa 'session_id' esplicito.",
                );
            }
        },
    };

    let rows = sqlx::query(
        "SELECT a.id, a.file_name, a.mime_type, a.size_bytes, a.kind, a.created_at \
         FROM chat_message_attachments a \
         JOIN chat_messages m ON m.id = a.message_id \
         WHERE m.session_id = $1 AND a.project_id = $2 \
         ORDER BY a.created_at ASC",
    )
    .bind(session_id)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await;

    match rows {
        Ok(rows) => {
            let mut items: Vec<Value> = Vec::with_capacity(rows.len());
            for r in rows {
                let id: Uuid = r.try_get("id").unwrap_or_else(|_| Uuid::nil());
                let file_name: String = r.try_get("file_name").unwrap_or_default();
                let mime_type: String = r.try_get("mime_type").unwrap_or_default();
                let size_bytes: i64 = r.try_get("size_bytes").unwrap_or(0);
                let kind: String = r.try_get("kind").unwrap_or_default();
                let created_at: chrono::DateTime<chrono::Utc> = r
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now());
                items.push(json!({
                    "id": id.to_string(),
                    "file_name": file_name,
                    "mime_type": mime_type,
                    "size_bytes": size_bytes,
                    "kind": kind,
                    "created_at": created_at.to_rfc3339(),
                }));
            }
            json!({ "session_id": session_id.to_string(), "count": items.len(), "attachments": items })
                .to_string()
        }
        Err(err) => {
            tracing::warn!(error=%err, "nexus_list_attachments: query fallita");
            err_json(format!("Errore lettura allegati: {err}"))
        }
    }
}

/// Legge un range di byte da un allegato e ritorna testo o base64.
///
/// Input: { "attachment_id": <uuid>, "encoding"?: "auto|text|base64",
///          "offset"?: u64, "length"?: usize }
pub async fn tool_nexus_read_attachment(ctx: &ToolContextCore, input: &Value) -> String {
    let attachment_id: Uuid = match input.get("attachment_id").and_then(Value::as_str) {
        Some(s) => match Uuid::parse_str(s) {
            Ok(u) => u,
            Err(_) => return err_json("Parametro 'attachment_id' non e' un UUID valido"),
        },
        None => return err_json("Parametro 'attachment_id' obbligatorio"),
    };

    let encoding_req = input
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .to_lowercase();
    if !matches!(encoding_req.as_str(), "auto" | "text" | "base64") {
        return err_json("Parametro 'encoding' deve essere uno di: auto|text|base64");
    }

    let offset: u64 = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let length_req: usize = input
        .get("length")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(MAX_READ_BYTES);

    // Look up dell'allegato.
    let row = sqlx::query(
        "SELECT file_path, mime_type, file_name, size_bytes \
         FROM chat_message_attachments \
         WHERE id = $1 AND project_id = $2",
    )
    .bind(attachment_id)
    .bind(ctx.project_id)
    .fetch_optional(&*ctx.db)
    .await;

    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) => {
            return err_json(format!(
                "Allegato {} non trovato nel progetto corrente",
                attachment_id
            ));
        }
        Err(err) => {
            tracing::warn!(error=%err, "nexus_read_attachment: lookup fallita");
            return err_json(format!("Errore lookup allegato: {err}"));
        }
    };

    let file_path: String = row.try_get("file_path").unwrap_or_default();
    let mime_type: String = row.try_get("mime_type").unwrap_or_default();
    let file_name: String = row.try_get("file_name").unwrap_or_default();
    let size_bytes: i64 = row.try_get("size_bytes").unwrap_or(0);

    if file_path.is_empty() {
        return err_json("file_path vuoto in DB per questo allegato");
    }

    let total_size = size_bytes.max(0) as u64;
    let effective_length = length_req.min(MAX_READ_BYTES);

    // FIX 2 (ADR 0012): deduplica via read_cache. La key include
    // attachment_id, offset, length, encoding richiesto. Cache hit > 1 = hint
    // al modello di cambiare strategia (passare a tool di estrazione struttur.).
    let cache_key = ReadCacheKey {
        attachment_id,
        kind: ReadKind::Attachment,
        entry_path: None,
        offset,
        length: effective_length as u64,
        encoding: encoding_req.clone(),
    };
    let file_path_owned = file_path.clone();
    let mime_type_owned = mime_type.clone();
    let file_name_owned = file_name.clone();
    let encoding_req_owned = encoding_req.clone();
    return read_cache::get_or_compute(&ctx.db, cache_key, move || async move {
        read_attachment_raw(
            attachment_id,
            file_path_owned,
            mime_type_owned,
            file_name_owned,
            offset,
            effective_length,
            total_size,
            encoding_req_owned,
        )
        .await
    })
    .await;
}

/// Lettura raw senza cache (chiamata dal closure di `read_cache::get_or_compute`).
#[allow(clippy::too_many_arguments)]
async fn read_attachment_raw(
    attachment_id: Uuid,
    file_path: String,
    mime_type: String,
    file_name: String,
    offset: u64,
    effective_length: usize,
    total_size: u64,
    encoding_req: String,
) -> String {
    let effective_length = if total_size > 0 {
        let remaining = total_size.saturating_sub(offset) as usize;
        effective_length.min(remaining)
    } else {
        effective_length
    };

    // Apri file e seek.
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(err) => {
            return err_json(format!(
                "Impossibile aprire il file '{}': {}",
                file_path, err
            ));
        }
    };
    if offset > 0 {
        if let Err(err) = file.seek(SeekFrom::Start(offset)).await {
            return err_json(format!("Seek fallita a offset {}: {}", offset, err));
        }
    }

    let mut buf: Vec<u8> = vec![0u8; effective_length];
    let read_bytes = match file.read(&mut buf).await {
        Ok(n) => n,
        Err(err) => {
            return err_json(format!("Errore lettura file: {}", err));
        }
    };
    buf.truncate(read_bytes);

    // Decisione encoding.
    let is_text_like = mime_type.starts_with("text/")
        || TEXT_LIKE_MIMES
            .iter()
            .any(|m| mime_type.eq_ignore_ascii_case(m));
    let encoding = match encoding_req.as_str() {
        "text" => "text",
        "base64" => "base64",
        _ => {
            if is_text_like {
                "text"
            } else {
                "base64"
            }
        }
    };

    let (content, encoding_label) = if encoding == "text" {
        match String::from_utf8(buf.clone()) {
            Ok(s) => (s, "text"),
            Err(_) => {
                // Fallback a base64 se i byte non sono UTF-8 validi.
                (
                    base64::engine::general_purpose::STANDARD.encode(&buf),
                    "base64",
                )
            }
        }
    } else {
        (
            base64::engine::general_purpose::STANDARD.encode(&buf),
            "base64",
        )
    };

    let truncated = (offset + read_bytes as u64) < total_size;

    json!({
        "id": attachment_id.to_string(),
        "name": file_name,
        "mime_type": mime_type,
        "encoding": encoding_label,
        "offset": offset,
        "length": read_bytes,
        "total_size": total_size,
        "truncated": truncated,
        "content": content,
    })
    .to_string()
}
