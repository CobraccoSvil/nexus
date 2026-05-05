//! `memory::memory_recent_writes` — recent memory_namespace updates.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct MemoryRecentWritesTool;

#[async_trait]
impl NexusToolHandler for MemoryRecentWritesTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let rows = sqlx::query(
            "SELECT namespace, key, updated_at FROM memory_namespace ORDER BY updated_at DESC LIMIT 25"
        ).fetch_all(&pool).await;
        match rows {
            Ok(rs) => {
                let items: Vec<Value> = rs.iter().map(|r| {
                    let ns: String = r.try_get("namespace").unwrap_or_default();
                    let key: String = r.try_get("key").unwrap_or_default();
                    json!({"namespace": ns, "key": key})
                }).collect();
                Ok(json!({"ok": true, "count": items.len(), "writes": items}))
            }
            Err(_) => Ok(json!({"ok": true, "count": 0, "note": "table missing"})),
        }
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
