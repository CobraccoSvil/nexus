//! `project_db_apply_migration` — applica le migration pending al DB del progetto utente.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;
use crate::nexus_tools::db_helper::get_pool;
use crate::project_db::{MigrationTool, ProjectDbContext, runner::MigrationRunner};

pub struct ProjectDbApplyMigrationTool;

#[async_trait]
impl NexusToolHandler for ProjectDbApplyMigrationTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let target_filename = args.get("filename").and_then(Value::as_str).map(String::from);

        let nexus_pool = get_pool().await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db connect: {}", e)))?;

        let config_row: Option<sqlx::postgres::PgRow> = sqlx::query(
            r#"SELECT migration_tool, migration_path, hosting_mode
               FROM project_database_config WHERE project_id = $1"#
        )
        .bind(ctx.project_id)
        .fetch_optional(&nexus_pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("config query: {}", e)))?;

        let Some(config) = config_row else {
            nexus_pool.close().await;
            return Ok(json!({"ok": false, "error": "Nessuna configurazione DB. Usa project_db_status."}));
        };

        let tool_str: Option<String> = config.try_get("migration_tool").unwrap_or(None);
        let migration_path: String = config.try_get::<Option<String>, _>("migration_path")
            .unwrap_or_default()
            .unwrap_or_else(|| "migrations".into());
        let hosting_mode: String = config.try_get("hosting_mode").unwrap_or_default();

        let project_conn_url = resolve_project_db_url(&ctx.project_id, &hosting_mode);

        let migration_tool = tool_str.as_deref()
            .and_then(MigrationTool::from_str)
            .unwrap_or(MigrationTool::GenericSql);

        let db_ctx = ProjectDbContext {
            project_root: ctx.project_root.clone(),
            project_id: ctx.project_id,
            migration_tool,
            migration_path,
        };
        let runner = MigrationRunner::new(db_ctx);

        let pending_rows: Vec<sqlx::postgres::PgRow> = sqlx::query(
            r#"SELECT id, filename, sql_diff FROM project_migration_history
               WHERE project_id = $1 AND status = 'pending' ORDER BY created_at ASC"#
        )
        .bind(ctx.project_id)
        .fetch_all(&nexus_pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("pending query: {}", e)))?;

        let rows_to_apply: Vec<&sqlx::postgres::PgRow> = pending_rows.iter()
            .filter(|row| {
                target_filename.as_deref().map_or(true, |fname| {
                    row.try_get::<String, _>("filename").map(|f| f == fname).unwrap_or(false)
                })
            })
            .collect();

        if rows_to_apply.is_empty() {
            nexus_pool.close().await;
            return Ok(json!({"ok": true, "applied": [], "message": "Nessuna migration pending."}));
        }

        let mut applied = Vec::new();
        for row in &rows_to_apply {
            let id: uuid::Uuid = row.try_get("id").unwrap_or(uuid::Uuid::nil());
            let filename: String = row.try_get("filename").unwrap_or_default();
            let sql_diff: Option<String> = row.try_get("sql_diff").unwrap_or(None);

            let apply_result = if let Some(sql) = &sql_diff {
                apply_raw_sql(&project_conn_url, sql).await
            } else {
                runner.apply_pending(&project_conn_url).await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            };

            match apply_result {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE project_migration_history SET status='applied', applied_at=NOW(), applied_by_agent='nexus-agent' WHERE id=$1"
                    ).bind(id).execute(&nexus_pool).await.ok();
                    applied.push(json!({"filename": &filename, "status": "applied"}));
                }
                Err(e) => {
                    sqlx::query(
                        "UPDATE project_migration_history SET status='failed', error_message=$2 WHERE id=$1"
                    ).bind(id).bind(&e).execute(&nexus_pool).await.ok();
                    nexus_pool.close().await;
                    return Ok(json!({
                        "ok": false,
                        "error": format!("Migration '{}' fallita: {}", filename, e),
                        "applied_before_failure": applied,
                    }));
                }
            }
        }

        nexus_pool.close().await;
        Ok(json!({"ok": true, "applied": applied}))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filename": {"type": "string", "description": "Applica solo questa migration. Se omesso, applica tutte le pending."}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: false, can_write_filesystem: false, can_execute_subproc: true, network_egress: true }
    }
}

fn resolve_project_db_url(project_id: &uuid::Uuid, hosting_mode: &str) -> String {
    let env_key = format!("PROJECT_{}_DB_URL", project_id.as_simple());
    if let Ok(url) = std::env::var(&env_key) { return url; }
    if hosting_mode == "internal" {
        format!("postgresql://nexus:nexus@proj-{}-db:5432/app", project_id.as_simple())
    } else {
        String::new()
    }
}

async fn apply_raw_sql(connection_url: &str, sql: &str) -> Result<(), String> {
    if connection_url.is_empty() { return Err("URL connessione progetto non configurata".into()); }
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect(connection_url).await
        .map_err(|e| format!("connect progetto: {}", e))?;
    sqlx::raw_sql(sql).execute(&pool).await
        .map_err(|e| format!("esecuzione SQL: {}", e))?;
    pool.close().await;
    Ok(())
}
