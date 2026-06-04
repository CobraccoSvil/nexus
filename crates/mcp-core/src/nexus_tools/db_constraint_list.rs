//! `database::db_constraint_list` — lista constraint in uno schema.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbConstraintListTool;

#[async_trait]
impl NexusToolHandler for DbConstraintListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = args
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q = "SELECT table_name, constraint_name, constraint_type \
                 FROM information_schema.table_constraints \
                 WHERE table_schema=$1 ORDER BY table_name, constraint_name";
        let rows = match sqlx::query(q).bind(&schema).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "table": r.try_get::<String, _>("table_name").unwrap_or_default(),
                    "name": r.try_get::<String, _>("constraint_name").unwrap_or_default(),
                    "type": r.try_get::<String, _>("constraint_type").unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({"ok": true, "schema": schema, "count": items.len(), "constraints": items}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"schema":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: true,
        }
    }
}
