//! `database::db_active_queries` — query attive da pg_stat_activity.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbActiveQueriesTool;

#[async_trait]
impl NexusToolHandler for DbActiveQueriesTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q = "SELECT pid, usename, datname, state, wait_event_type, wait_event, \
                        EXTRACT(EPOCH FROM (now() - query_start))::float AS duration_s, query \
                 FROM pg_stat_activity WHERE state IS NOT NULL AND state <> 'idle' \
                 ORDER BY query_start LIMIT 50";
        let rows = match sqlx::query(q).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows.iter().map(|r| json!({
            "pid": r.try_get::<i32, _>("pid").unwrap_or(0),
            "user": r.try_get::<Option<String>, _>("usename").unwrap_or_default(),
            "database": r.try_get::<Option<String>, _>("datname").unwrap_or_default(),
            "state": r.try_get::<Option<String>, _>("state").unwrap_or_default(),
            "wait_event_type": r.try_get::<Option<String>, _>("wait_event_type").unwrap_or_default(),
            "wait_event": r.try_get::<Option<String>, _>("wait_event").unwrap_or_default(),
            "duration_s": r.try_get::<Option<f64>, _>("duration_s").unwrap_or_default(),
            "query": r.try_get::<Option<String>, _>("query").unwrap_or_default(),
        })).collect();
        Ok(json!({"ok": true, "count": items.len(), "queries": items}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
