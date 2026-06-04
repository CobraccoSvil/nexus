//! `database::db_lock_list` — lock attivi da pg_locks (join con pg_stat_activity).
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbLockListTool;

#[async_trait]
impl NexusToolHandler for DbLockListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q =
            "SELECT l.locktype, l.mode, l.granted, a.pid, a.usename, a.application_name, a.state \
                 FROM pg_locks l LEFT JOIN pg_stat_activity a USING (pid) \
                 ORDER BY l.granted, l.pid LIMIT 200";
        let rows = match sqlx::query(q).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "locktype": r.try_get::<String, _>("locktype").unwrap_or_default(),
                    "mode": r.try_get::<String, _>("mode").unwrap_or_default(),
                    "granted": r.try_get::<bool, _>("granted").unwrap_or(false),
                    "pid": r.try_get::<Option<i32>, _>("pid").unwrap_or_default(),
                    "user": r.try_get::<Option<String>, _>("usename").unwrap_or_default(),
                    "app": r.try_get::<Option<String>, _>("application_name").unwrap_or_default(),
                    "state": r.try_get::<Option<String>, _>("state").unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({"ok": true, "count": items.len(), "locks": items}))
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
