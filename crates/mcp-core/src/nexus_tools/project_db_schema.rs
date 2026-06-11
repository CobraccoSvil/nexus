//! `project_db_schema` — inspect schema del DB del progetto.
//!
//! Equivalente a `db_schema_inspect` ma usa la connessione configurata in
//! `project_database_config` invece di `DATABASE_URL` di Nexus. Restituisce
//! tabelle + colonne (nome, tipo, nullable) per uno schema (default `public`)
//! o per una singola tabella se `table` e' specificato.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbSchemaTool;

#[async_trait]
impl NexusToolHandler for ProjectDbSchemaTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let schema = db_helper::schema_arg(args);
        let table_filter = args.get("table").and_then(Value::as_str).map(String::from);

        let project_pool = db_helper::open_project_pool(ctx).await?;

        let tables_out =
            db_helper::inspect_schema_tables(&project_pool, &schema, table_filter.as_deref(), true)
                .await?;

        project_pool.close().await;

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
                "schema": {
                    "type": "string",
                    "description": "Schema PostgreSQL da ispezionare. Default: public"
                },
                "table": {
                    "type": "string",
                    "description": "Se specificato, ritorna solo questa tabella"
                }
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
        let s = ProjectDbSchemaTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_write_filesystem);
    }

    #[test]
    fn test_input_schema_optional_table() {
        let s = ProjectDbSchemaTool.input_schema();
        assert!(s["properties"]["table"].is_object());
        assert!(s["properties"]["schema"].is_object());
    }
}
