//! Tool agente `nexus_search_semantic`: ricerca semantica unificata
//! su allegati, knowledge base, chat history, tool results cached.
//!
//! Fa parte della pipeline RAG strutturale (ADR 0015). Permette agli
//! agent di recuperare frammenti rilevanti senza ri-leggere interi file.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::rag::{self, SourceKind};

use super::AgentToolContext;

pub async fn tool_nexus_search_semantic(ctx: &AgentToolContext, input: &Value) -> String {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return json!({"error": "campo 'query' obbligatorio"}).to_string();
    }
    let top_k = input
        .get("top_k")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let kinds: Vec<SourceKind> = input
        .get("source_kinds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(SourceKind::parse))
                .collect()
        })
        .unwrap_or_default();
    let filter_attachment_id = input
        .get("filter_attachment_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let filter_session_id = input
        .get("filter_session_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());

    let mut extra: Vec<(String, Value)> = Vec::new();
    if let Some(att_id) = filter_attachment_id.clone() {
        extra.push(("source_id".to_string(), json!(att_id)));
    }

    match rag::search_semantic(
        &ctx.db,
        &query,
        kinds,
        Some(ctx.project_id),
        filter_session_id,
        top_k,
        extra,
    )
    .await
    {
        Ok(hits) => json!({
            "query": query,
            "count": hits.len(),
            "hits": hits,
        })
        .to_string(),
        Err(e) => {
            tracing::warn!("nexus_search_semantic: {}", e);
            json!({"error": format!("rag search fallita: {e}"), "hits": []}).to_string()
        }
    }
}
