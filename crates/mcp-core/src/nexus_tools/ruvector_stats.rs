//! `memory::ruvector_stats` — espone metriche correnti del database HNSW.
//!
//! Ritorna il numero di nodi, la media di neighbor per nodo (fan-out
//! effettivo del grafo), l'entry point, la dimensione vettoriale
//! dell'embedder, e la strategia consensus corrente.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_bridge::NexusBridge;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct RuVectorStatsTool;

#[async_trait]
impl NexusToolHandler for RuVectorStatsTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let Some(bridge) = NexusBridge::global() else {
            return Ok(json!({
                "ok": false,
                "reason": "bridge_not_initialized",
            }));
        };

        let stats = bridge.ruvector().stats();
        Ok(json!({
            "ok": true,
            "hnsw": {
                "total_nodes": stats.total_nodes,
                "avg_neighbors": stats.avg_neighbors,
                "entry_point": stats.entry_point,
            },
            "embedder_dim": bridge.embedder().dim(),
            "consensus_strategy": format!("{:?}", bridge.consensus().strategy()),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stats_call() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = RuVectorStatsTool.execute(&ctx, &json!({})).await.unwrap();
        // Deve sempre tornare un Value, con ok true o false
        assert!(out["ok"] == true || out["ok"] == false);
    }

    #[test]
    fn test_safety_readonly() {
        assert!(RuVectorStatsTool.safety().read_only);
    }
}
