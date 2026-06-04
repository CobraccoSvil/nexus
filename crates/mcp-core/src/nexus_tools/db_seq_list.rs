//! `database::db_seq_list` — lista sequence in uno schema.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbSeqListTool;

#[async_trait]
impl NexusToolHandler for DbSeqListTool {
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
            "SELECT sequence_name FROM information_schema.sequences WHERE sequence_schema=$1 ORDER BY sequence_name"
        ).bind(&schema).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let seqs: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get::<String, _>("sequence_name").ok())
            .collect();
        Ok(json!({"ok": true, "schema": schema, "count": seqs.len(), "sequences": seqs}))
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
