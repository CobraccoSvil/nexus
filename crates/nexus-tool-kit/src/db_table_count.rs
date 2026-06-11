//! `database::db_table_count` — `SELECT COUNT(*) FROM <table>`.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbTableCountTool;

#[async_trait]
impl NexusToolHandler for DbTableCountTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        // Punto unico extract_schema_table (regola L, S76).
        let (schema, table) = db_helper::extract_schema_table(args)?;
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q = format!(
            "SELECT COUNT(*)::bigint AS n FROM \"{}\".\"{}\"",
            schema, table
        );
        let row = match sqlx::query(&q).fetch_one(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let n: i64 = row.try_get("n").unwrap_or(-1);
        Ok(json!({"ok": true, "schema": schema, "table": table, "count": n}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["table"],"properties":{"schema":{"type":"string"},"table":{"type":"string"}}})
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
