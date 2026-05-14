//! `project_db_dump_schema` — pg_dump --schema-only del DB progetto.
//!
//! Comodo per snapshot pre-migration. Salva in `.nexus/backups/`.

use super::db_helper;
use super::project_db_backup::parse_dsn_parts;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbDumpSchemaTool;

#[async_trait]
impl NexusToolHandler for ProjectDbDumpSchemaTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let dsn = get_project_dsn(&nexus_pool, ctx.project_id).await?;
        nexus_pool.close().await;

        let (host, port, dbname, user, password) = parse_dsn_parts(&dsn)?;

        let backup_dir = ctx.project_root.join(".nexus").join("backups");
        tokio::fs::create_dir_all(&backup_dir)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("create backup dir: {}", e)))?;

        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("{}-schema-{}.sql", dbname, timestamp);
        let backup_path = backup_dir.join(&filename);
        let backup_path_str = backup_path.to_string_lossy().to_string();

        let start = std::time::Instant::now();

        let child = tokio::process::Command::new("pg_dump")
            .args([
                "-h", &host,
                "-p", &port,
                "-U", &user,
                "-d", &dbname,
                "--schema-only",
                "-f", &backup_path_str,
            ])
            .env("PGPASSWORD", &password)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("pg_dump: {}", e)))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        if child.status.success() {
            let size = tokio::fs::metadata(&backup_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);

            Ok(json!({
                "ok": true,
                "path": backup_path_str,
                "filename": filename,
                "database": dbname,
                "size_bytes": size,
                "duration_ms": duration_ms,
            }))
        } else {
            let stderr = String::from_utf8_lossy(&child.stderr);
            Ok(json!({
                "ok": false,
                "error": stderr.chars().take(2000).collect::<String>(),
            }))
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
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

async fn get_project_dsn(
    nexus_pool: &sqlx::PgPool,
    project_id: uuid::Uuid,
) -> Result<String, NexusToolError> {
    let row: Option<(Vec<u8>, String)> = sqlx::query_as(
        r#"SELECT connection_secret, engine
           FROM project_database_config
           WHERE project_id = $1
           ORDER BY is_primary DESC, created_at ASC
           LIMIT 1"#,
    )
    .bind(project_id)
    .fetch_optional(nexus_pool)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("lookup config: {}", e)))?;

    let (secret_bytes, engine) = row.ok_or_else(|| {
        NexusToolError::BadInput(format!(
            "Nessuna connessione DB per il progetto {}",
            project_id
        ))
    })?;

    if engine != "postgres" {
        return Err(NexusToolError::BadInput(format!(
            "Engine '{}' non supportato",
            engine
        )));
    }

    let dsn = String::from_utf8(secret_bytes)
        .map_err(|_| NexusToolError::BadInput("connection_secret non UTF-8".into()))?;

    db_helper::normalize_dsn_pub(dsn.trim())
        .map_err(|e| NexusToolError::BadInput(format!("DSN: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety() {
        let s = ProjectDbDumpSchemaTool.safety();
        assert!(s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }
}
