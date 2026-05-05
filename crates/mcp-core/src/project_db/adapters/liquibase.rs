//! Adapter Liquibase — crea changeset XML nella directory changelog.

use async_trait::async_trait;
use std::path::PathBuf;
use crate::project_db::{Migration, AppliedMigration, RolledBackMigration, ProjectDbError, ProjectDbContext};
use super::{MigrationAdapter, migration_timestamp, sha256_hex};

pub struct LiquibaseAdapter;

#[async_trait]
impl MigrationAdapter for LiquibaseAdapter {
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError> {
        let dir = ctx.project_root.join(&ctx.migration_path);
        if !dir.exists() { return Ok(vec![]); }
        let mut files: Vec<_> = std::fs::read_dir(&dir)?
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.ends_with(".xml") || n.ends_with(".sql") || n.ends_with(".yaml")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());
        let mut result = Vec::new();
        for entry in files {
            let path = entry.path();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            result.push(Migration {
                filename: filename.clone(),
                checksum: sha256_hex(&content),
                description: Some(filename.clone()),
                sql: None,
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
        let dir = ctx.project_root.join(&ctx.migration_path);
        std::fs::create_dir_all(&dir)?;
        let ts = migration_timestamp();
        let safe_name: String = name.chars()
            .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
            .collect();
        let filename = format!("{}_{}.sql", ts, safe_name);
        let path = dir.join(&filename);
        std::fs::write(&path, format!("-- Liquibase changeset: {}\n-- id: {}\n\n{}\n", name, ts, sql))?;
        Ok(path)
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let output = std::process::Command::new("liquibase")
            .args(["--url", connection_url, "update"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("liquibase update: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!("liquibase update fallita: {}", stderr)));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        let output = std::process::Command::new("liquibase")
            .args(["--url", connection_url, "rollbackCount", "1"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("liquibase rollback: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!("liquibase rollback fallita: {}", stderr)));
        }
        Ok(Some(RolledBackMigration { filename: "liquibase:last".into() }))
    }
}
