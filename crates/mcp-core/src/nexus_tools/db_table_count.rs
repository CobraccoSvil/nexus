//! `database::db_table_count` — `SELECT COUNT(*) FROM <table>`.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbTableCountTool;

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[async_trait]
impl NexusToolHandler for DbTableCountTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let table = args
            .get("table")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("table required".into()))?;
        let schema = args
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public");
        if !ident_ok(table) || !ident_ok(schema) {
            return Err(NexusToolError::BadInput("invalid identifier".into()));
        }
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
