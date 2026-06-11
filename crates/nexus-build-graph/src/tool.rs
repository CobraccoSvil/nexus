//! Tool MCP `nexus_build_graph_info` — espone la mappa del build graph al
//! modello come strumento di prima classe.
//!
//! Routed da `nexus_builtin::execute` (handler dedicato).

use serde_json::{json, Value};
use uuid::Uuid;

use super::cache::BuildGraphCache;

/// Handler MCP per `nexus_build_graph_info`.
///
/// Input: `{ "project_id": "<uuid>" }` (opzionale: se assente usa il project_id
/// del contesto runtime, passato dal dispatcher).
/// Output JSON: `BuildGraphInfo` serializzato.
pub async fn handle_build_graph_info(default_project_id: Uuid, arguments: &Value) -> String {
    let project_id = arguments
        .get("project_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or(default_project_id);

    let cache = match BuildGraphCache::global() {
        Some(c) => c,
        None => {
            return json!({
                "error": "BuildGraphCache non inizializzato (build_graph::init_global non chiamato)",
            })
            .to_string();
        }
    };

    match cache.get_or_compute(project_id).await {
        Ok(info) => serde_json::to_string(&info).unwrap_or_else(|e| {
            json!({"error": format!("serializzazione fallita: {}", e)}).to_string()
        }),
        Err(e) => json!({
            "error": format!("build_graph compute fallito: {}", e),
            "project_id": project_id.to_string(),
        })
        .to_string(),
    }
}
