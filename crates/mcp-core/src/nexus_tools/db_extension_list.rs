//! `database::db_extension_list` — lista estensioni installate da pg_extension.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbExtensionListTool;

#[async_trait]
impl NexusToolHandler for DbExtensionListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let rows =
            match sqlx::query("SELECT extname, extversion FROM pg_extension ORDER BY extname")
                .fetch_all(&pool)
                .await
            {
                Ok(r) => r,
                Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
            };
        let items: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "name": r.try_get::<String, _>("extname").unwrap_or_default(),
                    "version": r.try_get::<String, _>("extversion").unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({"ok": true, "count": items.len(), "extensions": items}))
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
