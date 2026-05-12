//! `project_db_connections` — restituisce le connessioni DB configurate per il progetto corrente.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;
use crate::nexus_tools::db_helper::get_pool;

pub struct ProjectDbConnectionsTool;

#[async_trait]
impl NexusToolHandler for ProjectDbConnectionsTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = get_pool().await
            .map_err(|e| NexusToolError::BadInput(format!("db connect: {}", e)))?;

        let rows = sqlx::query(
            r#"SELECT name, engine, hosting_mode, is_primary,
                      ENCODE(connection_secret, 'escape') AS connection_string,
                      migration_tool, migration_path, allow_ddl_override
               FROM project_database_config
               WHERE project_id = $1
               ORDER BY is_primary DESC, LOWER(name)"#
        )
        .bind(ctx.project_id)
        .fetch_all(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("query: {}", e)))?;

        pool.close().await;

        if rows.is_empty() {
            return Ok(json!({
                "ok": true,
                "configured": false,
                "connections": [],
                "message": "Nessuna connessione DB configurata per questo progetto."
            }));
        }

        let connections: Vec<Value> = rows.iter().map(|r| {
            let name: String = r.try_get("name").unwrap_or_default();
            let engine: Option<String> = r.try_get("engine").unwrap_or(None);
            let hosting: Option<String> = r.try_get("hosting_mode").unwrap_or(None);
            let primary: bool = r.try_get("is_primary").unwrap_or(false);
            let dsn: Option<String> = r.try_get("connection_string").unwrap_or(None);
            let migration_tool: Option<String> = r.try_get("migration_tool").unwrap_or(None);
            let migration_path: Option<String> = r.try_get("migration_path").unwrap_or(None);
            let ddl_override: bool = r.try_get("allow_ddl_override").unwrap_or(false);

            json!({
                "name": name,
                "engine": engine,
                "hosting_mode": hosting,
                "is_primary": primary,
                "connection_string": dsn,
                "migration_tool": migration_tool,
                "migration_path": migration_path,
                "allow_ddl_override": ddl_override,
            })
        }).collect();

        Ok(json!({
            "ok": true,
            "configured": true,
            "connections": connections,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
