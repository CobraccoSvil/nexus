//! `database::db_foreign_keys` — lista foreign key in uno schema.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbForeignKeysTool;

#[async_trait]
impl NexusToolHandler for DbForeignKeysTool {
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
        let q = "SELECT tc.table_name, tc.constraint_name, kcu.column_name, \
                        ccu.table_name AS ref_table, ccu.column_name AS ref_column \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                 JOIN information_schema.constraint_column_usage ccu \
                   ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema \
                 WHERE tc.constraint_type='FOREIGN KEY' AND tc.table_schema=$1 \
                 ORDER BY tc.table_name, tc.constraint_name";
        let rows = match sqlx::query(q).bind(&schema).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "table": r.try_get::<String, _>("table_name").unwrap_or_default(),
                    "constraint": r.try_get::<String, _>("constraint_name").unwrap_or_default(),
                    "column": r.try_get::<String, _>("column_name").unwrap_or_default(),
                    "ref_table": r.try_get::<String, _>("ref_table").unwrap_or_default(),
                    "ref_column": r.try_get::<String, _>("ref_column").unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({"ok": true, "schema": schema, "count": items.len(), "foreign_keys": items}))
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
