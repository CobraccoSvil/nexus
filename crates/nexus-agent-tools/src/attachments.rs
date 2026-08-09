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
    let messaggio: String = msg.into();
    crate::errore_json(messaggio)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_core::{NoopEmbedder, NoopMutationHooks};
    use nexus_types::tool_outcome::{is_tool_failure, EsitoTool, RispostaTool};
    use std::sync::Arc;

    /// Contesto reale (la struct di produzione). Il pool e' lazy e non viene mai
    /// contattato: i rami d'errore qui esercitati rifiutano l'input PRIMA di
    /// toccare il DB, ed e' il percorso che il modello incontra per primo quando
    /// sbaglia una chiamata.
    fn ctx_di_prova(root: std::path::PathBuf) -> ToolContextCore {
        let db =
            sqlx::PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy");
        ToolContextCore {
            root_path: root,
            user_id: Uuid::nil(),
            is_git_repo: false,
            can_write: true,
            project_id: Uuid::nil(),
            session_id: None,
            db: Arc::new(db.clone()),
            run_db: Arc::new(db),
            parent_run_id: None,
            run_id: None,
            long_running_patterns: Vec::new(),
            user_role: "admin".to_string(),
            is_nexus_operator: true,
            project_channels: Arc::new(dashmap::DashMap::new()),
            monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            hooks: Arc::new(NoopMutationHooks),
            embedder: Arc::new(NoopEmbedder),
            isolated_subrun: false,
            write_scope: Vec::new(),
        }
    }

    /// I tool che rispondono in JSON dichiaravano il fallimento SOLO nel campo
    /// `error` del corpo: al confine del dispatch quel corpo diventa una `String`
    /// e `RispostaTool::da_testo_legacy` non ha nulla da leggere, quindi il tool
    /// risulta RIUSCITO. Un allegato che rifiuta l'estrazione a ogni tentativo
    /// diventa cosi' una ripetizione produttiva, e l'anti-loop la classifica come
    /// stallo invece che come causa radice da diagnosticare (regola M).
    ///
    /// Il test attraversa i PRODUTTORI veri — le funzioni che il dispatch chiama —
    /// e arriva alla CONSEGUENZA, l'esito nel campo, non alla stringa.
    /// Mutazione che rende rosso: togliere `crate::errore_json` da `err_json`,
    /// oppure dal ramo `attachment_id` mancante di `tool_nexus_extract_pdf_text`.
    #[tokio::test]
    async fn l_errore_di_un_tool_json_e_dichiarato_alla_macchina() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf());

        // I tool JSON ANCORA LEGACY: dichiarano col marker, e l'esito lo
        // ricostruisce il ponte al confine del dispatch.
        let legacy: Vec<(&str, String)> = vec![
            (
                "nexus_read_attachment",
                tool_nexus_read_attachment(&ctx, &json!({})).await,
            ),
            (
                "nexus_list_archive_entries",
                crate::archive_tools::tool_nexus_list_archive_entries(&ctx, &json!({})).await,
            ),
        ];

        for (nome, uscita) in legacy {
            assert!(
                is_tool_failure(&uscita),
                "{nome} non dichiara il fallimento alla macchina: {uscita}"
            );
            assert_eq!(
                RispostaTool::da_testo_legacy(uscita.clone()).esito,
                EsitoTool::Fallito,
                "{nome}: il confine del dispatch lo legge come RIUSCITO: {uscita}"
            );
            // Il messaggio per l'umano resta nel corpo: il marker aggiunge una
            // dichiarazione, non sostituisce la spiegazione.
            assert!(uscita.contains("attachment_id"), "{nome}: {uscita}");
        }

        // I tool MIGRATI: la stessa proprieta', dichiarata dove non si puo'
        // perdere. Il marker non c'e' piu' e non deve esserci — il corpo torna
        // a essere un JSON integro — quindi il criterio e' il CAMPO, e in piu'
        // c'e' la natura, che il canale legacy non poteva trasportare.
        let migrati: Vec<(&str, RispostaTool)> = vec![(
            "nexus_extract_pdf_text",
            crate::document_tools::tool_nexus_extract_pdf_text(&ctx, &json!({})).await,
        )];

        for (nome, uscita) in migrati {
            assert_eq!(
                uscita.esito,
                EsitoTool::Fallito,
                "{nome} non dichiara il fallimento nel campo: {uscita:?}"
            );
            assert_eq!(
                uscita.natura,
                Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
                "{nome}: un parametro mancante lo corregge l'agente: {uscita:?}"
            );
            assert!(uscita.testo.contains("attachment_id"), "{nome}: {uscita:?}");
            assert!(
                !is_tool_failure(&uscita.testo),
                "{nome}: il marker non deve piu' comparire nel testo di un tool migrato,                  o il corpo JSON resta spezzato: {uscita:?}"
            );
        }
    }

    /// Il percorso di SUCCESSO non e' toccato, e non e' un dettaglio: il presidio
    /// del budget letture allegati
    /// (`nexus-agent-graph::decisions::tool_dispatch::extract_returned_bytes`)
    /// deserializza questa stringa e cerca l'intero `length` di primo livello. Un
    /// marker in testa renderebbe il corpo non deserializzabile, quindi la
    /// proprieta' va fissata qui, sul produttore vero: se un domani una lettura
    /// riuscita venisse marcata, il budget smetterebbe di contare i byte letti e
    /// nessun test dell'altro crate se ne accorgerebbe, perche' li' l'input e'
    /// costruito a mano (regola O).
    #[tokio::test]
    async fn la_lettura_riuscita_resta_un_json_con_length_intero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("nota.txt");
        std::fs::write(&file, "0123456789").expect("seed");

        let uscita = read_attachment_raw(
            Uuid::nil(),
            file.to_string_lossy().into_owned(),
            "text/plain".to_string(),
            "nota.txt".to_string(),
            0,
            MAX_READ_BYTES,
            10,
            "auto".to_string(),
        )
        .await;

        assert!(
            !is_tool_failure(&uscita),
            "lettura riuscita marcata come fallita: {uscita}"
        );
        let corpo: Value = serde_json::from_str(&uscita).expect("il successo resta JSON integro");
        assert_eq!(
            corpo.get("length").and_then(Value::as_i64),
            Some(10),
            "il campo su cui il budget conta i byte letti: {uscita}"
        );
    }

    /// La faccia opposta: una lettura FALLITA non deve poter dichiarare byte
    /// letti. Il budget ne contava gia' zero — nessun `length` nel corpo
    /// d'errore — e continua a contarne zero: il marker non apre una scorciatoia.
    #[tokio::test]
    async fn la_lettura_fallita_non_dichiara_byte_letti() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uscita = read_attachment_raw(
            Uuid::nil(),
            dir.path().join("assente.bin").to_string_lossy().into_owned(),
            "application/octet-stream".to_string(),
            "assente.bin".to_string(),
            0,
            MAX_READ_BYTES,
            10,
            "auto".to_string(),
        )
        .await;

        assert!(
            is_tool_failure(&uscita),
            "apertura fallita non dichiarata: {uscita}"
        );
        assert!(
            !uscita.contains("\"length\""),
            "un errore non porta byte letti: {uscita}"
        );
    }
}
