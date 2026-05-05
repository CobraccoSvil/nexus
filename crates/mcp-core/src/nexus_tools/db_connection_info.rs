//! `database::db_connection_info` — info connessione corrente (user, db, server version).
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct DbConnectionInfoTool;

#[async_trait]
impl NexusToolHandler for DbConnectionInfoTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = match db_helper::get_pool().await {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "error": e})),
        };
        let row = match sqlx::query(
            "SELECT current_user AS usr, current_database() AS db, inet_server_addr()::text AS host, inet_server_port() AS port, version() AS ver"
        ).fetch_one(&pool).await {
            Ok(r) => r,
            Err(e) => return Ok(json!({"ok": false, "error": format!("query: {}", e)})),
        };
        Ok(json!({
            "ok": true,
            "user": row.try_get::<String, _>("usr").unwrap_or_default(),
            "database": row.try_get::<String, _>("db").unwrap_or_default(),
            "host": row.try_get::<Option<String>, _>("host").unwrap_or_default(),
            "port": row.try_get::<Option<i32>, _>("port").unwrap_or_default(),
            "version": row.try_get::<String, _>("ver").unwrap_or_default(),
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
