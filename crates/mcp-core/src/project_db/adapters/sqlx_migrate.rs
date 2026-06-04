//! Adapter sqlx-migrate (Rust) — crea migration tramite `sqlx migrate add`.

use super::{sha256_hex, MigrationAdapter};
use crate::project_db::{
    AppliedMigration, Migration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct SqlxMigrateAdapter;

#[async_trait]
impl MigrationAdapter for SqlxMigrateAdapter {
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError> {
        let dir = ctx.project_root.join(&ctx.migration_path);
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut files: Vec<_> = std::fs::read_dir(&dir)?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".sql"))
            .collect();
        files.sort_by_key(|e| e.file_name());
        let mut result = Vec::new();
        for entry in files {
            let path = entry.path();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            result.push(Migration {
                filename: filename.clone(),
                checksum: sha256_hex(&content),
                description: Some(filename.clone()),
                sql: Some(content),
            });
        }
        Ok(result)
    }

    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        sql: &str,
    ) -> Result<PathBuf, ProjectDbError> {
        let safe_name: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // Prova sqlx CLI
        let output = std::process::Command::new("sqlx")
            .args(["migrate", "add", &safe_name])
            .current_dir(&ctx.project_root)
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                // Trova il file appena creato
                let dir = ctx.project_root.join(&ctx.migration_path);
                let mut files: Vec<_> = std::fs::read_dir(&dir)?
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".sql"))
                    .collect();
                files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
                if let Some(last) = files.last() {
                    // Scrivi SQL nel file creato da sqlx
                    std::fs::write(last.path(), sql)?;
                    return Ok(last.path());
                }
            }
        }
        // Fallback: crea file manualmente con numerazione sqlx (YYYYMMDDHHMMSS_nome.sql)
        let dir = ctx.project_root.join(&ctx.migration_path);
        std::fs::create_dir_all(&dir)?;
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let filename = format!("{}_{}.sql", ts, safe_name);
        let path = dir.join(&filename);
        std::fs::write(&path, sql)?;
        Ok(path)
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let output = std::process::Command::new("sqlx")
            .args(["migrate", "run"])
            .env("DATABASE_URL", connection_url)
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("sqlx migrate run: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "sqlx migrate run fallita: {}",
                stderr
            )));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        let output = std::process::Command::new("sqlx")
            .args(["migrate", "revert"])
            .env("DATABASE_URL", connection_url)
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("sqlx migrate revert: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "sqlx migrate revert fallita: {}",
                stderr
            )));
        }
        Ok(Some(RolledBackMigration {
            filename: "sqlx:last".into(),
        }))
    }
}
