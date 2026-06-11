//! `memory::memory_evict_stats` — eviction stats from memory_namespace.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct MemoryEvictStatsTool;

#[async_trait]
impl NexusToolHandler for MemoryEvictStatsTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        // Heuristic: count rows older than 30 days
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS cnt FROM memory_namespace WHERE updated_at < NOW() - INTERVAL '30 days'"
        ).fetch_one(&pool).await;
        match row {
            Ok(r) => {
                let cnt: i64 = r.try_get("cnt").unwrap_or(0);
                Ok(json!({"ok": true, "evictable_old_rows": cnt, "ttl_days": 30}))
            }
            Err(_) => Ok(json!({"ok": true, "evictable_old_rows": 0, "note": "table missing"})),
        }
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
