//! `project_db_status` — stato del database e migration pending per un progetto utente.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::db_helper::get_pool;
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectDbStatusTool;

#[async_trait]
impl NexusToolHandler for ProjectDbStatusTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pool = get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("db connect: {}", e)))?;

        let config_row = sqlx::query(
            r#"SELECT engine, hosting_mode, migration_tool, migration_path, allow_ddl_override
               FROM project_database_config WHERE project_id = $1"#,
        )
        .bind(ctx.project_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("config query: {}", e)))?;

        let Some(config) = config_row else {
            pool.close().await;
            return Ok(json!({
                "ok": true,
                "configured": false,
                "message": "Nessuna configurazione DB trovata. Configura il DB nel wizard di importazione."
            }));
        };

        let engine: String = config.try_get("engine").unwrap_or_default();
        let hosting_mode: String = config.try_get("hosting_mode").unwrap_or_default();
        let migration_tool: Option<String> = config.try_get("migration_tool").unwrap_or(None);
        let migration_path: Option<String> = config.try_get("migration_path").unwrap_or(None);
        let allow_override: bool = config.try_get("allow_ddl_override").unwrap_or(false);

        let counts = sqlx::query(
            r#"SELECT status, COUNT(*)::bigint as cnt
               FROM project_migration_history WHERE project_id = $1 GROUP BY status"#,
        )
        .bind(ctx.project_id)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();

        let mut by_status = serde_json::Map::new();
        let mut pending_count = 0i64;
        for row in &counts {
            let status: String = row.try_get("status").unwrap_or_default();
            let cnt: i64 = row.try_get("cnt").unwrap_or(0);
            if status == "pending" {
                pending_count = cnt;
            }
            by_status.insert(status, json!(cnt));
        }

        pool.close().await;

        Ok(json!({
            "ok": true,
            "configured": true,
            "engine": engine,
            "hosting_mode": hosting_mode,
            "migration_tool": migration_tool,
            "migration_path": migration_path,
            "allow_ddl_override": allow_override,
            "pending_migrations": pending_count,
            "migrations_by_status": by_status,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
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
