//! `database::db_dead_tuples` — top tabelle per dead tuples (n_dead_tup).
use super::db_helper::{self, CatalogBind, CatalogCol};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbDeadTuplesTool;

#[async_trait]
impl NexusToolHandler for DbDeadTuplesTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let limit = db_helper::limit_arg(args, 20, 200);
        let q = "SELECT schemaname, relname AS table, n_live_tup, n_dead_tup, last_autovacuum::text AS last_autovacuum \
                 FROM pg_stat_user_tables ORDER BY n_dead_tup DESC LIMIT $1";
        let items = match db_helper::list_catalog_rows(
            q,
            CatalogBind::Int(limit),
            &[
                CatalogCol::text("schemaname", "schema"),
                CatalogCol::text("table", "table"),
                CatalogCol::int("n_live_tup", "live"),
                CatalogCol::int("n_dead_tup", "dead"),
                CatalogCol::text_opt("last_autovacuum", "last_autovacuum"),
            ],
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        Ok(json!({"ok": true, "count": items.len(), "tables": items}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"limit":{"type":"integer"}}})
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
