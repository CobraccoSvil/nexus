//! `database::db_dead_tuples` — top tabelle per dead tuples (n_dead_tup).
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbDeadTuplesTool;

#[async_trait]
impl NexusToolHandler for DbDeadTuplesTool {
    async fn execute(&self, _ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20).clamp(1, 200);
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q = "SELECT schemaname, relname AS table, n_live_tup, n_dead_tup, last_autovacuum::text AS last_autovacuum \
                 FROM pg_stat_user_tables ORDER BY n_dead_tup DESC LIMIT $1";
        let rows = match sqlx::query(q).bind(limit).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows.iter().map(|r| json!({
            "schema": r.try_get::<String, _>("schemaname").unwrap_or_default(),
            "table": r.try_get::<String, _>("table").unwrap_or_default(),
            "live": r.try_get::<i64, _>("n_live_tup").unwrap_or(0),
            "dead": r.try_get::<i64, _>("n_dead_tup").unwrap_or(0),
            "last_autovacuum": r.try_get::<Option<String>, _>("last_autovacuum").unwrap_or_default(),
        })).collect();
        Ok(json!({"ok": true, "count": items.len(), "tables": items}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"limit":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
