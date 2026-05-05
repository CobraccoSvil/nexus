//! `database::db_size` — dimensione totale del database corrente.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbSizeTool;

#[async_trait]
impl NexusToolHandler for DbSizeTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let row = match sqlx::query(
            "SELECT current_database() AS name, pg_database_size(current_database())::bigint AS bytes, pg_size_pretty(pg_database_size(current_database())) AS pretty"
        ).fetch_one(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        Ok(json!({
            "ok": true,
            "database": row.try_get::<String, _>("name").unwrap_or_default(),
            "bytes": row.try_get::<i64, _>("bytes").unwrap_or(0),
            "pretty": row.try_get::<String, _>("pretty").unwrap_or_default(),
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
