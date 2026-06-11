//! `database::db_foreign_keys` — lista foreign key in uno schema.
use super::db_helper::{self, CatalogBind, CatalogCol};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbForeignKeysTool;

#[async_trait]
impl NexusToolHandler for DbForeignKeysTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = db_helper::schema_arg(args);
        let q = "SELECT tc.table_name, tc.constraint_name, kcu.column_name, \
                        ccu.table_name AS ref_table, ccu.column_name AS ref_column \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                 JOIN information_schema.constraint_column_usage ccu \
                   ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema \
                 WHERE tc.constraint_type='FOREIGN KEY' AND tc.table_schema=$1 \
                 ORDER BY tc.table_name, tc.constraint_name";
        let items = match db_helper::list_catalog_rows(
            q,
            CatalogBind::Text(schema.clone()),
            &[
                CatalogCol::text("table_name", "table"),
                CatalogCol::text("constraint_name", "constraint"),
                CatalogCol::text("column_name", "column"),
                CatalogCol::text("ref_table", "ref_table"),
                CatalogCol::text("ref_column", "ref_column"),
            ],
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
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
