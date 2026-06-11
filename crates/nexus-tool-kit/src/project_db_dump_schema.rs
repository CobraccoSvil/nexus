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

        // Punto unico in nexus_tools::run_pg_dump (regola L, S74).
        let outcome = super::run_pg_dump(
            &[
                "-h",
                &host,
                "-p",
                &port,
                "-U",
                &user,
                "-d",
                &dbname,
                "--schema-only",
                "-f",
                &backup_path_str,
            ],
            &password,
            None,
            &backup_path,
        )
        .await?;
        if outcome.success {
            Ok(json!({
                "ok": true,
                "path": backup_path_str,
                "filename": filename,
                "database": dbname,
                "size_bytes": outcome.size_bytes,
                "duration_ms": outcome.duration_ms,
            }))
        } else {
            Ok(json!({ "ok": false, "error": outcome.stderr_truncated }))
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

// get_project_dsn: punto unico in nexus_tools::project_db_helpers (regola L).
use super::project_db_helpers::get_project_dsn;

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
