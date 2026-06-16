//! Search semantica RAG: embed query + search Qdrant filtrato.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::{current_config, qdrant_client, RagError, SourceKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub source_kind: String,
    pub source_id: String,
    pub chunk_index: i64,
    pub chunk_text: String,
    pub score: f32,
    pub metadata: Value,
}

/// Embedding della query tramite l'embedder ONNX in-process del bridge
/// (regola L: punto unico, niente round-trip HTTP/gRPC verso il brain Python).
/// `embed_one` e' sincrono/CPU-bound, quindi viene avvolto in `spawn_blocking`.
async fn embed_query(query: &str) -> Result<Vec<f32>, RagError> {
    let bridge = crate::nexus_bridge::NexusBridge::global()
        .ok_or_else(|| RagError::Embed("nexus bridge non inizializzato".into()))?;
    let q = query.to_string();
    tokio::task::spawn_blocking(move || bridge.embed_one(&q))
        .await
        .map_err(|e| RagError::Embed(format!("embed_query spawn_blocking join: {e}")))
}

/// Cerca i top-K chunk piu' rilevanti per `query` filtrati su:
/// - `source_kinds`: lista di SourceKind ammessi (default: tutti tranne Code).
/// - `project_id`: se Some, filtra payload.project_id.
/// - `session_id`: se Some, filtra payload.session_id (rilevante per chat_history).
/// - `extra_filters`: ulteriori filtri payload arbitrari (es. ("source_id", "<uuid>")).
pub async fn search_semantic(
    db: &PgPool,
    query: &str,
    source_kinds: Vec<SourceKind>,
    project_id: Option<Uuid>,
    session_id: Option<Uuid>,
    top_k: Option<usize>,
    extra_filters: Vec<(String, Value)>,
) -> Result<Vec<SearchHit>, RagError> {
    let cfg = current_config(db).await?;
    if !cfg.enabled {
        return Err(RagError::Disabled);
    }
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let top_k = top_k.unwrap_or(cfg.top_k_default).clamp(1, 100);
    let kinds = if source_kinds.is_empty() {
        vec![
            SourceKind::Attachment,
            SourceKind::Kb,
            SourceKind::ChatHistory,
            SourceKind::ToolResult,
        ]
    } else {
        source_kinds
    };

    let http = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| RagError::Embed(format!("reqwest: {e}")))?;
    let query_vec = embed_query(query).await?;

    let mut all_hits: Vec<SearchHit> = Vec::new();
    for kind in kinds {
        let collection = cfg.collection_for(kind).to_string();
        let mut filters: Vec<(String, Value)> = Vec::new();
        // Filtri per-kind: alcune collection legacy non hanno project_id
        // (Conversation usa session_id; MetaDoc e' globale).
        if let Some(p) = project_id {
            if kind.supports_project_filter() {
                filters.push(("project_id".to_string(), json!(p.to_string())));
            }
        }
        if let Some(s) = session_id {
            if kind.uses_session_filter() {
                filters.push(("session_id".to_string(), json!(s.to_string())));
            }
        }
        for (k, v) in extra_filters.iter() {
            filters.push((k.clone(), v.clone()));
        }
        let hits = match qdrant_client::search_points(
            &http,
            &cfg.qdrant_url,
            &collection,
            query_vec.clone(),
            top_k,
            filters,
        )
        .await
        {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(
                    "rag.search_semantic: collection '{}' fallita: {}",
                    collection,
                    e
                );
                continue;
            }
        };
        for h in hits {
            let p = h.payload;
            // Estrazione testo flessibile: il RAG framework usa `chunk_text`,
            // ma le collection legacy hanno schemi diversi (conversation_context
            // -> `content`, nexus_meta_docs -> `body_md`/`title`,
            // prompt_corrections -> `correction`/`text`). Proviamo in ordine.
            let chunk_text = p
                .get("chunk_text")
                .or_else(|| p.get("content"))
                .or_else(|| p.get("body_md"))
                .or_else(|| p.get("correction"))
                .or_else(|| p.get("text"))
                .or_else(|| p.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let chunk_index = p.get("chunk_index").and_then(|v| v.as_i64()).unwrap_or(0);
            // source_id: prova source_id, poi note_id/id specifici delle legacy.
            let source_id = p
                .get("source_id")
                .or_else(|| p.get("note_id"))
                .or_else(|| p.get("doc_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let metadata = p.get("metadata").cloned().unwrap_or(Value::Null);
            all_hits.push(SearchHit {
                source_kind: kind.as_str().to_string(),
                source_id,
                chunk_index,
                chunk_text,
                score: h.score,
                metadata,
            });
        }
    }
    all_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_hits.truncate(top_k);
    tracing::info!(
        "rag.search_semantic: query_len={} hits={}",
        query.chars().count(),
        all_hits.len()
    );
    Ok(all_hits)
}
