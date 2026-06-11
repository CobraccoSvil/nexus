//! `project_db_rollback` — annulla l'ultima migration applicata al DB del progetto utente.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_tools::db_helper::get_pool;
use crate::project_db::{runner::MigrationRunner, MigrationTool, ProjectDbContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectDbRollbackTool;

#[async_trait]
impl NexusToolHandler for ProjectDbRollbackTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let nexus_pool = get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let config_row: Option<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT migration_tool, migration_path, hosting_mode FROM project_database_config WHERE project_id=$1"
        )
        .bind(ctx.project_id)
        .fetch_optional(&nexus_pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("config query: {}", e)))?;

        let Some(config) = config_row else {
            nexus_pool.close().await;
            return Ok(json!({"ok": false, "error": "Nessuna configurazione DB trovata."}));
        };

        let tool_str: Option<String> = config.try_get("migration_tool").unwrap_or(None);
        let migration_path: String = config
            .try_get::<Option<String>, _>("migration_path")
            .unwrap_or_default()
            .unwrap_or_else(|| "migrations".into());
        let hosting_mode: String = config.try_get("hosting_mode").unwrap_or_default();

        let project_conn_url = if hosting_mode == "internal" {
            format!(
                "postgresql://nexus:nexus@proj-{}-db:5432/app",
                ctx.project_id.as_simple()
            )
        } else {
            std::env::var(format!("PROJECT_{}_DB_URL", ctx.project_id.as_simple()))
                .unwrap_or_default()
        };

        let last_row: Option<sqlx::postgres::PgRow> = sqlx::query(
            r#"SELECT id, filename, rollback_sql FROM project_migration_history
               WHERE project_id=$1 AND status='applied' ORDER BY applied_at DESC LIMIT 1"#,
        )
        .bind(ctx.project_id)
        .fetch_optional(&nexus_pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("last applied: {}", e)))?;

        let Some(last) = last_row else {
            nexus_pool.close().await;
            return Ok(json!({"ok": true, "message": "Nessuna migration applicata da annullare."}));
        };

        let migration_id: uuid::Uuid = last.try_get("id").unwrap_or(uuid::Uuid::nil());
        let filename: String = last.try_get("filename").unwrap_or_default();
        let rollback_sql: Option<String> = last.try_get("rollback_sql").unwrap_or(None);

        let rollback_result = if let Some(sql) = &rollback_sql {
            apply_raw_sql(&project_conn_url, sql).await
        } else {
            let migration_tool = tool_str
                .as_deref()
                .and_then(MigrationTool::from_str)
                .unwrap_or(MigrationTool::GenericSql);
            let db_ctx = ProjectDbContext {
                project_root: ctx.project_root.clone(),
                migration_tool,
                migration_path,
            };
            let runner = MigrationRunner::new(db_ctx);
            runner
                .rollback_last(&project_conn_url)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        };

        match rollback_result {
            Ok(()) => {
                sqlx::query(
                    "UPDATE project_migration_history SET status='rolled_back', rolled_back_at=NOW() WHERE id=$1"
                ).bind(migration_id).execute(&nexus_pool).await.ok();
                nexus_events::dispatcher::emit_global(
                    ctx.project_id,
                    nexus_events::ProjectEvent::MigrationRolledBack {
                        migration_name: filename.clone(),
                        version: migration_id.to_string(),
                    },
                );
                nexus_pool.close().await;
                Ok(json!({"ok": true, "rolled_back": filename}))
            }
            Err(e) => {
                nexus_pool.close().await;
                Ok(json!({"ok": false, "error": format!("Rollback '{}' fallito: {}", filename, e)}))
            }
        }
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

async fn apply_raw_sql(url: &str, sql: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("URL connessione progetto non configurata".into());
    }
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(url)
        .await
        .map_err(|e| format!("connect: {}", e))?;
    sqlx::raw_sql(sql)
        .execute(&pool)
        .await
        .map_err(|e| format!("rollback SQL: {}", e))?;
    pool.close().await;
    Ok(())
}
