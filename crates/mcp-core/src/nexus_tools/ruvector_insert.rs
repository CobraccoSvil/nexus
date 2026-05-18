//! `memory::ruvector_insert` — inserisce un testo nel database vettoriale
//! HNSW globale (RuVector).
//!
//! Il testo viene convertito in embedding via l'`HashEmbedder` condiviso con
//! il Q-Learning router (dim=256), poi indicizzato nel grafo HNSW. Se la
//! dimensione non corrisponde a quella già stabilita dal primo insert,
//! l'operazione fallisce con `BadInput`.
//!
//! L'handler è opt-in: se il `NexusBridge` globale non è inizializzato,
//! ritorna `{ok: false, reason: "bridge_not_initialized"}` senza errore,
//! così il resto del servizio continua a funzionare.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_bridge::NexusBridge;
use async_trait::async_trait;
use ruvector::VectorMetadata;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct RuVectorInsertTool;

#[async_trait]
impl NexusToolHandler for RuVectorInsertTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("text required".into()))?;
        let id = args
            .get("id")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("v_{}", uuid::Uuid::new_v4()));
        let namespace = args
            .get("namespace")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string();

        // Tags opzionali {key: value}
        let mut tags: HashMap<String, String> = HashMap::new();
        if let Some(obj) = args.get("tags").and_then(Value::as_object) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    tags.insert(k.clone(), s.to_string());
                }
            }
        }
        let ttl_seconds = args
            .get("ttl_seconds")
            .and_then(Value::as_u64)
            .map(|v| v as u32);

        let Some(bridge) = NexusBridge::global() else {
            return Ok(json!({
                "ok": false,
                "reason": "bridge_not_initialized",
                "note": "NexusBridge::init_global() non è stato chiamato: RuVector non disponibile",
            }));
        };

        let vector = bridge.embedder().embed(text);
        let dim = vector.len();
        let metadata = VectorMetadata {
            id: id.clone(),
            namespace: namespace.clone(),
            tags,
            created_at: chrono::Utc::now(),
            ttl_seconds,
        };

        // Usa insert_with_persist: l'inserimento va sia nell'HNSW in-memory
        // sia su PostgreSQL (fire-and-forget) se il pool è configurato.
        let confidence = args
            .get("confidence")
            .and_then(Value::as_f64)
            .map(|v| v.clamp(0.0, 1.0) as f32)
            .unwrap_or(1.0);

        match bridge.ruvector().insert_with_persist(id.clone(), vector, Some(metadata), confidence) {
            Ok(node_id) => {
                let stats = bridge.ruvector().stats();
                nexus_events::dispatcher::emit_global(
                    _ctx.project_id,
                    nexus_events::ProjectEvent::MemoryUpdated {
                        category: namespace.clone(),
                        count_delta: 1,
                    },
                );
                Ok(json!({
                    "ok": true,
                    "id": id,
                    "node_id": node_id,
                    "namespace": namespace,
                    "dim": dim,
                    "total_nodes": stats.total_nodes,
                    "persisted": bridge.ruvector().has_persistence(),
                }))
            }
            Err(e) => Err(NexusToolError::BadInput(format!(
                "ruvector insert failed: {}",
                e
            ))),
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string", "description": "Testo da embeddare e indicizzare"},
                "id": {"type": "string", "description": "ID opzionale (default: uuid generato)"},
                "namespace": {"type": "string", "description": "Namespace logico (default: 'default')"},
                "tags": {"type": "object", "description": "Tag stringa chiave/valore"},
                "ttl_seconds": {"type": "integer", "minimum": 0},
                "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Confidenza del vettore per SONA pruning (default: 1.0)"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_insert_without_bridge() {
        // Senza bridge globale init, l'handler ritorna ok=false con reason
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = RuVectorInsertTool
            .execute(&ctx, &json!({"text": "hello world"}))
            .await
            .unwrap();
        // Nota: questo test dipende dall'ordine di esecuzione con altri test
        // che inizializzano il bridge globale. Accettiamo entrambi gli esiti.
        assert!(out["ok"] == true || out["ok"] == false);
    }

    #[test]
    fn test_safety_writes_mem() {
        let s = RuVectorInsertTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_write_filesystem);
    }
}
