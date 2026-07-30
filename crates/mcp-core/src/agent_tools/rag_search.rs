//! Tool agente `nexus_search_semantic`: ricerca semantica unificata
//! su allegati, knowledge base, chat history, tool results cached.
//!
//! Fa parte della pipeline RAG strutturale (ADR 0015). Permette agli
//! agent di recuperare frammenti rilevanti senza ri-leggere interi file.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::rag::{self, SourceKind};

use super::AgentToolContext;
use nexus_types::tool_outcome::tool_failure;

/// Costruisce l'esito FALLITO del tool: marker + payload JSON (contratto
/// `nexus_types::tool_outcome`). Senza il marker in testa questi fallimenti
/// erano indistinguibili da una ricerca riuscita per anti-loop/supervisore/
/// final_gate, che leggono solo `is_tool_failure`.
fn search_failure(payload: Value) -> String {
    tool_failure(payload.to_string())
}

pub async fn tool_nexus_search_semantic(ctx: &AgentToolContext, input: &Value) -> String {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return search_failure(json!({"error": "campo 'query' obbligatorio"}));
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
            search_failure(json!({"error": format!("rag search fallita: {e}"), "hits": []}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_failure_dichiara_il_fallimento_e_preserva_il_payload() {
        // Chiama il PRODUTTORE reale usato dai 2 rami di errore del tool
        // (query mancante, ricerca semantica fallita).
        let out = search_failure(json!({"error": "rag search fallita: qdrant down", "hits": []}));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
        let after_marker = out
            .trim_start_matches(nexus_types::tool_outcome::TOOL_FAILURE_MARKER)
            .trim_start();
        let parsed: Value =
            serde_json::from_str(after_marker).expect("payload dopo il marker e' JSON valido");
        assert_eq!(parsed["error"], "rag search fallita: qdrant down");
    }
}
