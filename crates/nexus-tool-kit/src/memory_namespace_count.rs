//! `memory::memory_namespace_count` — count distinct memory namespaces in DB.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct MemoryNamespaceCountTool;

#[async_trait]
impl NexusToolHandler for MemoryNamespaceCountTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let row =
            sqlx::query("SELECT COUNT(DISTINCT namespace)::bigint AS c FROM memory_namespace")
                .fetch_one(&pool)
                .await;
        match row {
            Ok(r) => {
                let c: i64 = r.try_get("c").unwrap_or(0);
                Ok(json!({"ok": true, "namespaces": c}))
            }
            Err(_) => Ok(json!({"ok": true, "namespaces": 0, "note": "table missing"})),
        }
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
