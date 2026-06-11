//! `database::db_role_list` — lista ruoli da pg_roles.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbRoleListTool;

#[async_trait]
impl NexusToolHandler for DbRoleListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let rows = match sqlx::query(
            "SELECT rolname, rolsuper, rolcanlogin FROM pg_roles ORDER BY rolname",
        )
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
                    "name": r.try_get::<String, _>("rolname").unwrap_or_default(),
                    "super": r.try_get::<bool, _>("rolsuper").unwrap_or(false),
                    "can_login": r.try_get::<bool, _>("rolcanlogin").unwrap_or(false),
                })
            })
            .collect();
        Ok(json!({"ok": true, "count": items.len(), "roles": items}))
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
