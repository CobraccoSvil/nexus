//! `database::db_table_list` — lista tabelle in uno schema.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbTableListTool;

#[async_trait]
impl NexusToolHandler for DbTableListTool {
    async fn execute(&self, _ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let schema = args.get("schema").and_then(Value::as_str).unwrap_or("public").to_string();
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let rows = match sqlx::query(
            "SELECT tablename FROM pg_tables WHERE schemaname=$1 ORDER BY tablename"
        ).bind(&schema).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let tables: Vec<String> = rows.iter().filter_map(|r| r.try_get::<String, _>("tablename").ok()).collect();
        Ok(json!({"ok": true, "schema": schema, "count": tables.len(), "tables": tables}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"schema":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
