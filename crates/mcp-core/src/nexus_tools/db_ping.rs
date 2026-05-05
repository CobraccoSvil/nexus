//! `database::db_ping` — verifica connettività `SELECT 1` su DATABASE_URL.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

pub struct DbPingTool;

#[async_trait]
impl NexusToolHandler for DbPingTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let db_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return Ok(json!({"ok": false, "error": "DATABASE_URL not set"})),
        };
        let start = std::time::Instant::now();
        let pool = match PgPoolOptions::new().max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5)).connect(&db_url).await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": format!("connect: {}", e)})),
        };
        let row = match sqlx::query("SELECT 1::int AS one").fetch_one(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        let one: i32 = row.try_get("one").unwrap_or(0);
        Ok(json!({"ok": one == 1, "one": one, "latency_ms": start.elapsed().as_millis() as u64}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
