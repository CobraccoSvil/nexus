//! `database::db_view_list` — lista views in uno schema.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbViewListTool;

#[async_trait]
impl NexusToolHandler for DbViewListTool {
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
        let rows = match sqlx::query(
            "SELECT viewname FROM pg_views WHERE schemaname=$1 ORDER BY viewname",
        )
        .bind(&schema)
        .fetch_all(&pool)
        .await
        {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let views: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get::<String, _>("viewname").ok())
            .collect();
        Ok(json!({"ok": true, "schema": schema, "count": views.len(), "views": views}))
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
