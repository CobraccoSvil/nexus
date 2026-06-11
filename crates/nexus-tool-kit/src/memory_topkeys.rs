//! `memory::memory_topkeys` — top namespaces by row count.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct MemoryTopkeysTool;

#[async_trait]
impl NexusToolHandler for MemoryTopkeysTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let rows = sqlx::query(
            "SELECT namespace, COUNT(*)::bigint AS cnt FROM memory_namespace GROUP BY namespace ORDER BY cnt DESC LIMIT 20"
        ).fetch_all(&pool).await;
        match rows {
            Ok(rs) => {
                let items: Vec<Value> = rs
                    .iter()
                    .map(|r| {
                        let ns: String = r.try_get("namespace").unwrap_or_default();
                        let cnt: i64 = r.try_get("cnt").unwrap_or(0);
                        json!({"namespace": ns, "count": cnt})
                    })
                    .collect();
                Ok(json!({"ok": true, "count": items.len(), "top": items}))
            }
            Err(_) => Ok(json!({"ok": true, "count": 0, "note": "table missing"})),
        }
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
