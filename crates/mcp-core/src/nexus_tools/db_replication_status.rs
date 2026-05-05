//! `database::db_replication_status` — stato replication da pg_stat_replication.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbReplicationStatusTool;

#[async_trait]
impl NexusToolHandler for DbReplicationStatusTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q = "SELECT pid, usename, application_name, client_addr::text AS client, state, sync_state \
                 FROM pg_stat_replication ORDER BY pid";
        let rows = match sqlx::query(q).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows.iter().map(|r| json!({
            "pid": r.try_get::<i32, _>("pid").unwrap_or(0),
            "user": r.try_get::<Option<String>, _>("usename").unwrap_or_default(),
            "app": r.try_get::<Option<String>, _>("application_name").unwrap_or_default(),
            "client": r.try_get::<Option<String>, _>("client").unwrap_or_default(),
            "state": r.try_get::<Option<String>, _>("state").unwrap_or_default(),
            "sync_state": r.try_get::<Option<String>, _>("sync_state").unwrap_or_default(),
        })).collect();
        Ok(json!({"ok": true, "count": items.len(), "replicas": items}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
