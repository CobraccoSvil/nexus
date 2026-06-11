//! `database::db_schema_inspect` — introspezione schema PostgreSQL.
//!
//! Usa la stessa connessione globale di mcp-core (variabile d'ambiente
//! `DATABASE_URL`) per leggere `information_schema.tables` e `.columns`
//! filtrate per schema (default `public`).
//!
//! Output: `{schema, tables: [{name, columns: [{name, type, nullable}]}]}`.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbSchemaInspectTool;

#[async_trait]
impl NexusToolHandler for DbSchemaInspectTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = db_helper::schema_arg(args);
        let table_filter = args.get("table").and_then(Value::as_str).map(String::from);

        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };

        let tables_out =
            db_helper::inspect_schema_tables(&pool, &schema, table_filter.as_deref(), false)
                .await?;

        pool.close().await;

        Ok(json!({
            "ok": true,
            "schema": schema,
            "table_count": tables_out.len(),
            "tables": tables_out,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "schema": {"type": "string", "default": "public"},
                "table": {"type": "string", "description": "Filtra una sola tabella"}
            }
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_readonly_network() {
        let s = DbSchemaInspectTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_structure() {
        let s = DbSchemaInspectTool.input_schema();
        assert!(s["properties"]["schema"].is_object());
    }
}
