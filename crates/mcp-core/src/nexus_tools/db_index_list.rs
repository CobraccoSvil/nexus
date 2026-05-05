//! `database::db_index_list` — lista index in uno schema.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbIndexListTool;

#[async_trait]
impl NexusToolHandler for DbIndexListTool {
    async fn execute(&self, _ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let schema = args.get("schema").and_then(Value::as_str).unwrap_or("public").to_string();
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let rows = match sqlx::query(
            "SELECT tablename, indexname, indexdef FROM pg_indexes WHERE schemaname=$1 ORDER BY tablename, indexname"
        ).bind(&schema).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows.iter().map(|r| json!({
            "table": r.try_get::<String, _>("tablename").unwrap_or_default(),
            "name": r.try_get::<String, _>("indexname").unwrap_or_default(),
            "definition": r.try_get::<String, _>("indexdef").unwrap_or_default(),
        })).collect();
        Ok(json!({"ok": true, "schema": schema, "count": items.len(), "indexes": items}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"schema":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
