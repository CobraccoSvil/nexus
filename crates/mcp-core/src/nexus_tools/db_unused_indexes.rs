//! `database::db_unused_indexes` — index con `idx_scan = 0` da pg_stat_user_indexes.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbUnusedIndexesTool;

#[async_trait]
impl NexusToolHandler for DbUnusedIndexesTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q = "SELECT schemaname, relname AS table, indexrelname AS index, idx_scan \
                 FROM pg_stat_user_indexes \
                 WHERE idx_scan = 0 ORDER BY schemaname, relname, indexrelname";
        let rows = match sqlx::query(q).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows.iter().map(|r| json!({
            "schema": r.try_get::<String, _>("schemaname").unwrap_or_default(),
            "table": r.try_get::<String, _>("table").unwrap_or_default(),
            "index": r.try_get::<String, _>("index").unwrap_or_default(),
            "scans": r.try_get::<i64, _>("idx_scan").unwrap_or(0),
        })).collect();
        Ok(json!({"ok": true, "count": items.len(), "indexes": items}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
