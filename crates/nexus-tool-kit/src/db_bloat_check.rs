//! `database::db_bloat_check` — stima rapida bloat: rapporto dead/live per tabella.
use super::db_helper::{self, CatalogBind, CatalogCol};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DbBloatCheckTool;

#[async_trait]
impl NexusToolHandler for DbBloatCheckTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let limit = db_helper::limit_arg(args, 20, 200);
        let q = "SELECT schemaname, relname AS table, n_live_tup, n_dead_tup, \
                        CASE WHEN n_live_tup>0 THEN (n_dead_tup::float / n_live_tup::float) ELSE 0 END AS ratio \
                 FROM pg_stat_user_tables WHERE n_live_tup > 0 \
                 ORDER BY ratio DESC LIMIT $1";
        let items = match db_helper::list_catalog_rows(
            q,
            CatalogBind::Int(limit),
            &[
                CatalogCol::text("schemaname", "schema"),
                CatalogCol::text("table", "table"),
                CatalogCol::int("n_live_tup", "live"),
                CatalogCol::int("n_dead_tup", "dead"),
                CatalogCol::float("ratio", "dead_ratio"),
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
