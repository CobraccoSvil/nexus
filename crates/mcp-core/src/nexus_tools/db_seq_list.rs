//! `database::db_seq_list` — lista sequence in uno schema.
use super::db_helper::{self, CatalogBind};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbSeqListTool;

#[async_trait]
impl NexusToolHandler for DbSeqListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = db_helper::schema_arg(args);
        let seqs = match db_helper::list_catalog_strings(
            "SELECT sequence_name FROM information_schema.sequences WHERE sequence_schema=$1 ORDER BY sequence_name",
            CatalogBind::Text(schema.clone()),
            "sequence_name",
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
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
