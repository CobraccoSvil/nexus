//! Indexer RAG: chunking + embed batch (in-process) + upsert Qdrant.

use std::path::PathBuf;

use reqwest::Client;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::{chunker, current_config, qdrant_client, RagError, SourceKind};

/// Embedding di un batch di testi tramite l'embedder ONNX in-process del bridge
/// (regola L: punto unico, niente round-trip HTTP/gRPC verso il brain Python).
/// `embed_many` e' sincrono/CPU-bound, quindi viene avvolto in `spawn_blocking`.
async fn embed_batch(texts: &[String]) -> Result<Vec<Vec<f32>>, RagError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let bridge = crate::nexus_bridge::NexusBridge::global()
        .ok_or_else(|| RagError::Embed("nexus bridge non inizializzato".into()))?;
    // Clona i testi prima dello spawn (move) per evitare problemi di lifetime;
    // i `&str` vengono ricostruiti dentro il closure.
    let owned: Vec<String> = texts.to_vec();
    tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        bridge.embed_many(&refs)
    })
    .await
    .map_err(|e| RagError::Embed(format!("embed_batch spawn_blocking join: {e}")))
}

pub async fn index_text(
    db: &PgPool,
    source_kind: SourceKind,
    source_id: &str,
    project_id: Option<Uuid>,
    session_id: Option<Uuid>,
    text: &str,
    metadata: Value,
) -> Result<usize, RagError> {
    let cfg = current_config(db).await?;
    if !cfg.enabled {
        return Err(RagError::Disabled);
    }
    if text.trim().is_empty() {
        return Ok(0);
    }

    let chunks = chunker::chunk_text(text, cfg.chunk_size, cfg.chunk_overlap);
    if chunks.is_empty() {
        return Ok(0);
    }
    tracing::info!(
        "rag.index_text: kind={} source_id={} chunks={} chars={}",
        source_kind.as_str(),
        source_id,
        chunks.len(),
        text.chars().count()
    );

    let http = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| RagError::Embed(format!("reqwest build: {e}")))?;

    let vectors = embed_batch(&chunks).await?;
    if vectors.len() != chunks.len() {
        return Err(RagError::Embed(format!(
            "mismatch vectors={} chunks={}",
            vectors.len(),
            chunks.len()
        )));
    }
    if let Some(v0) = vectors.first() {
        if v0.len() != cfg.embedding_dim {
            tracing::warn!(
                "rag.index_text: dim ricevuta {} != config {}",
                v0.len(),
                cfg.embedding_dim
            );
        }
    }

    let collection = cfg.collection_for(source_kind).to_string();
    qdrant_client::ensure_collection(&http, &cfg.qdrant_url, &collection, cfg.embedding_dim)
        .await?;

    let mut points = Vec::with_capacity(chunks.len());

    for (idx, (chunk_text, vector)) in chunks.iter().zip(vectors.iter()).enumerate() {
        let name = format!("{}::{}::{}", source_kind.as_str(), source_id, idx);
        let pid = stable_point_id(&name);
        let mut payload = json!({
            "source_kind": source_kind.as_str(),
            "source_id": source_id,
            "chunk_index": idx,
            "chunk_text": chunk_text,
        });
        if let Some(p) = project_id {
            payload["project_id"] = json!(p.to_string());
        }
        if let Some(s) = session_id {
            payload["session_id"] = json!(s.to_string());
        }
        if !metadata.is_null() {
            payload["metadata"] = metadata.clone();
        }
        points.push(json!({
            "id": pid,
            "vector": vector,
            "payload": payload,
        }));
    }

    qdrant_client::upsert_points(&http, &cfg.qdrant_url, &collection, points).await?;
    Ok(chunks.len())
}

pub async fn index_attachment(
    db: &PgPool,
    attachment_id: Uuid,
    file_path: PathBuf,
    mime_type: String,
    name_hint: String,
    project_id: Option<Uuid>,
    session_id: Option<Uuid>,
) -> Result<usize, RagError> {
    let mime = mime_type.to_lowercase();
    let lower = name_hint.to_lowercase();

    // Politica "mai troncare-e-buttare": le funzioni inline estraggono l'INTERO
    // contenuto, che viene poi chunked e indicizzato integralmente nel RAG.
    let text: Option<String> = if mime == "application/pdf" || lower.ends_with(".pdf") {
        crate::agent_tools::document_tools::extract_pdf_text_inline(&file_path)
            .await
            .ok()
    } else if mime.contains("wordprocessingml") || lower.ends_with(".docx") {
        crate::agent_tools::document_tools::extract_docx_text_inline(&file_path)
            .await
            .ok()
    } else if mime == "application/zip"
        || lower.ends_with(".zip")
        || lower.ends_with(".fig")
        || lower.ends_with(".make")
    {
        crate::agent_tools::figma_tools::extract_figma_strings_inline(&file_path)
            .await
            .ok()
    } else if mime.starts_with("text/")
        || mime == "application/json"
        || lower.ends_with(".md")
        || lower.ends_with(".txt")
        || lower.ends_with(".json")
    {
        tokio::fs::read_to_string(&file_path).await.ok()
    } else {
        None
    };

    let Some(text) = text else {
        tracing::info!(
            "rag.index_attachment: id={} mime={} non estraibile, skip",
            attachment_id,
            mime
        );
        return Ok(0);
    };
    if text.trim().is_empty() {
        return Ok(0);
    }

    let metadata = json!({
        "mime_type": mime,
        "name": name_hint,
    });
    let n = index_text(
        db,
        SourceKind::Attachment,
        &attachment_id.to_string(),
        project_id,
        session_id,
        &text,
        metadata,
    )
    .await?;

    // Separazione DB: chat_message_attachments e' una tabella migrata. La risolviamo
    // sul pool del progetto (per project_id se presente, altrimenti per session_id);
    // senza ne' project_id ne' session_id resta il meta (comportamento storico per
    // allegati senza scope). Il marcatore indexed_at e' best-effort (l'indice
    // Qdrant e' gia' scritto): DB progetto non disponibile -> update saltato con
    // WARN, niente fallback al meta-DB (la riga sul meta non esiste).
    let attach_pool = if let Some(pid) = project_id {
        match crate::project_db_routes::project_data_pool_from(db, pid).await {
            Ok(pool) => Some(pool),
            Err(e) => {
                tracing::warn!(project_id = %pid, error = %e, "rag.index_attachment: DB progetto non disponibile, marcatore indexed_at saltato");
                None
            }
        }
    } else if let Some(sid) = session_id {
        match crate::project_db_routes::project_data_pool_by_session_from(db, sid).await {
            Ok(pool) => Some(pool),
            Err(e) => {
                tracing::warn!(session_id = %sid, error = %e, "rag.index_attachment: DB progetto non disponibile, marcatore indexed_at saltato");
                None
            }
        }
    } else {
        Some(db.clone())
    };
    if let Some(attach_pool) = attach_pool {
        if let Err(e) = sqlx::query(
            "UPDATE chat_message_attachments SET indexed_at = NOW(), chunk_count = $2 WHERE id = $1",
        )
        .bind(attachment_id)
        .bind(n as i32)
        .execute(&attach_pool)
        .await
        {
            tracing::warn!(
                "rag.index_attachment: update indexed_at fallito id={} err={}",
                attachment_id,
                e
            );
        }
    }
    tracing::info!(
        "rag.index_attachment: id={} chunks={} indicizzati",
        attachment_id,
        n
    );
    Ok(n)
}

/// ID stabile per un punto Qdrant: SHA-256(name) -> primi 16 bytes -> UUID v4-shape.
/// Cosi' reindicizzare la stessa source+chunk_index sovrascrive il punto.
fn stable_point_id(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let h = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&h[..16]);
    // Set version=4, variant RFC4122 per compatibilita' Qdrant UUID format.
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}
