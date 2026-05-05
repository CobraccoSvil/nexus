//! `database::db_bloat_check` — stima rapida bloat: rapporto dead/live per tabella.
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbBloatCheckTool;

#[async_trait]
impl NexusToolHandler for DbBloatCheckTool {
    async fn execute(&self, _ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20).clamp(1, 200);
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let q = "SELECT schemaname, relname AS table, n_live_tup, n_dead_tup, \
                        CASE WHEN n_live_tup>0 THEN (n_dead_tup::float / n_live_tup::float) ELSE 0 END AS ratio \
                 FROM pg_stat_user_tables WHERE n_live_tup > 0 \
                 ORDER BY ratio DESC LIMIT $1";
        let rows = match sqlx::query(q).bind(limit).fetch_all(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let items: Vec<Value> = rows.iter().map(|r| json!({
            "schema": r.try_get::<String, _>("schemaname").unwrap_or_default(),
            "table": r.try_get::<String, _>("table").unwrap_or_default(),
            "live": r.try_get::<i64, _>("n_live_tup").unwrap_or(0),
            "dead": r.try_get::<i64, _>("n_dead_tup").unwrap_or(0),
            "dead_ratio": r.try_get::<f64, _>("ratio").unwrap_or(0.0),
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
