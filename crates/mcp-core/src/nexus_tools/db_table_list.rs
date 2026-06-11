//! `database::db_table_list` — lista tabelle in uno schema.
use super::db_helper::{self, CatalogBind};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbTableListTool;

#[async_trait]
impl NexusToolHandler for DbTableListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = db_helper::schema_arg(args);
        let tables = match db_helper::list_catalog_strings(
            "SELECT tablename FROM pg_tables WHERE schemaname=$1 ORDER BY tablename",
            CatalogBind::Text(schema.clone()),
            "tablename",
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        Ok(json!({"ok": true, "schema": schema, "count": tables.len(), "tables": tables}))
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
