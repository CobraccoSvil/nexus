//! `project_db_create_migration` — crea un file migration timestampato per il DB del progetto utente.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_tools::db_helper::get_pool;
use crate::project_db::adapters::sha256_hex;
use crate::project_db::{runner::MigrationRunner, MigrationTool, ProjectDbContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectDbCreateMigrationTool;

#[async_trait]
impl NexusToolHandler for ProjectDbCreateMigrationTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("parametro 'name' obbligatorio".into()))?;
        let sql = args
            .get("sql")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("parametro 'sql' obbligatorio".into()))?;
        let description = args
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(name);

        let pool = get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("db connect: {}", e)))?;

        let config_row: Option<sqlx::postgres::PgRow> = sqlx::query(
            r#"SELECT migration_tool, migration_path
               FROM project_database_config WHERE project_id = $1"#,
        )
        .bind(ctx.project_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("config query: {}", e)))?;

        let Some(config) = config_row else {
            pool.close().await;
            return Ok(json!({
                "ok": false,
                "error": "DDL_BLOCKED",
                "message": "Nessuna configurazione DB trovata. Configura prima il DB del progetto.",
                "suggested_tool": "project_db_status"
            }));
        };

        let tool_str: Option<String> = config.try_get("migration_tool").unwrap_or(None);
        let migration_path: String = config
            .try_get::<Option<String>, _>("migration_path")
            .unwrap_or_default()
            .unwrap_or_else(|| "migrations".into());
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
        // Sempre false: il SQL della migration DEVE poter contenere DDL (CREATE/ALTER/…).
        // allow_ddl_override riguarda solo DDL ad-hoc via API override / shell, non i file migration.
        let file_path = runner
            .create_migration(name, sql, false)
            .await
            .map_err(|e| NexusToolError::BadInput(e.to_string()))?;

        let filename = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let content = std::fs::read_to_string(&file_path).unwrap_or_else(|_| sql.to_string());
        let checksum = sha256_hex(&content);

        sqlx::query(
            r#"INSERT INTO project_migration_history
               (project_id, filename, checksum, status, description, sql_diff, created_by_agent)
               VALUES ($1, $2, $3, 'pending', $4, $5, 'nexus-agent')
               ON CONFLICT (project_id, filename) DO NOTHING"#,
        )
        .bind(ctx.project_id)
        .bind(&filename)
        .bind(&checksum)
        .bind(description)
        .bind(sql)
        .execute(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert history: {}", e)))?;

        pool.close().await;

        Ok(json!({
            "ok": true,
            "filename": filename,
            "path": file_path.to_string_lossy(),
            "checksum": checksum,
            "status": "pending",
            "message": format!("Migration '{}' creata. Usa project_db_apply_migration per applicarla.", filename)
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "sql"],
            "properties": {
                "name": {"type": "string"},
                "sql": {"type": "string"},
                "description": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: true,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}
