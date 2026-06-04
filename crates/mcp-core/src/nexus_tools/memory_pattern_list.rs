//! `memory::memory_pattern_list` — list known reasoning patterns from DB.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct MemoryPatternListTool;

#[async_trait]
impl NexusToolHandler for MemoryPatternListTool {
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
            "SELECT pattern_id, kind FROM reasoning_bank ORDER BY created_at DESC LIMIT 50",
        )
        .fetch_all(&pool)
        .await;
        match rows {
            Ok(rs) => {
                let items: Vec<Value> = rs
                    .iter()
                    .map(|r| {
                        let id: String = r.try_get("pattern_id").unwrap_or_default();
                        let kind: Option<String> = r.try_get("kind").ok();
                        json!({"id": id, "kind": kind})
                    })
                    .collect();
                Ok(json!({"ok": true, "count": items.len(), "patterns": items}))
            }
            Err(_) => Ok(json!({"ok": true, "count": 0, "note": "table missing"})),
        }
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
