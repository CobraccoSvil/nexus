//! `database::db_index_list` — lista index in uno schema.
use super::db_helper::{self, CatalogBind, CatalogCol};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbIndexListTool;

#[async_trait]
impl NexusToolHandler for DbIndexListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = db_helper::schema_arg(args);
        let items = match db_helper::list_catalog_rows(
            "SELECT tablename, indexname, indexdef FROM pg_indexes WHERE schemaname=$1 ORDER BY tablename, indexname",
            CatalogBind::Text(schema.clone()),
            &[
                CatalogCol::text("tablename", "table"),
                CatalogCol::text("indexname", "name"),
                CatalogCol::text("indexdef", "definition"),
            ],
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        Ok(json!({"ok": true, "schema": schema, "count": items.len(), "indexes": items}))
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
