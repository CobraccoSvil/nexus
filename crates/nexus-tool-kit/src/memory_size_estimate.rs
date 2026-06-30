//! `memory::memory_size_estimate` — rough size of memory_namespace table.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct MemorySizeEstimateTool;

#[async_trait]
impl NexusToolHandler for MemorySizeEstimateTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let row = sqlx::query("SELECT pg_total_relation_size('memory_namespace')::bigint AS sz")
            .fetch_one(&pool)
            .await;
        match row {
            Ok(r) => {
                let sz: i64 = r.try_get("sz").unwrap_or(0);
                Ok(json!({"ok": true, "table_bytes": sz}))
            }
            Err(_) => Ok(json!({"ok": true, "table_bytes": 0, "note": "table missing"})),
        }
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
