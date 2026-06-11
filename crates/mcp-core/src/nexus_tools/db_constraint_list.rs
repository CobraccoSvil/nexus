//! `database::db_constraint_list` — lista constraint in uno schema.
use super::db_helper::{self, CatalogBind, CatalogCol};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbConstraintListTool;

#[async_trait]
impl NexusToolHandler for DbConstraintListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = db_helper::schema_arg(args);
        let q = "SELECT table_name, constraint_name, constraint_type \
                 FROM information_schema.table_constraints \
                 WHERE table_schema=$1 ORDER BY table_name, constraint_name";
        let items = match db_helper::list_catalog_rows(
            q,
            CatalogBind::Text(schema.clone()),
            &[
                CatalogCol::text("table_name", "table"),
                CatalogCol::text("constraint_name", "name"),
                CatalogCol::text("constraint_type", "type"),
            ],
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        Ok(json!({"ok": true, "schema": schema, "count": items.len(), "constraints": items}))
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
