//! Adapter Knex (Node.js) — crea migration tramite `npx knex migrate:make`.

use async_trait::async_trait;
use std::path::PathBuf;
use crate::project_db::{Migration, AppliedMigration, RolledBackMigration, ProjectDbError, ProjectDbContext};
use super::{MigrationAdapter, sha256_hex};

pub struct KnexAdapter;

#[async_trait]
impl MigrationAdapter for KnexAdapter {
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError> {
        let dir = ctx.project_root.join(&ctx.migration_path);
        if !dir.exists() { return Ok(vec![]); }
        let mut files: Vec<_> = std::fs::read_dir(&dir)?
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.ends_with(".js") || n.ends_with(".ts")
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
        _sql: &str,
    ) -> Result<PathBuf, ProjectDbError> {
        let safe_name: String = name.chars()
            .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
            .collect();
        let output = std::process::Command::new("npx")
            .args(["knex", "migrate:make", &safe_name])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("knex migrate:make: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!("knex migrate:make fallita: {}", stderr)));
        }
        Ok(ctx.project_root.join(&ctx.migration_path))
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let output = std::process::Command::new("npx")
            .args(["knex", "migrate:latest"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("knex migrate:latest: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!("knex migrate:latest fallita: {}", stderr)));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        let output = std::process::Command::new("npx")
            .args(["knex", "migrate:rollback"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("knex migrate:rollback: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!("knex migrate:rollback fallita: {}", stderr)));
        }
        Ok(Some(RolledBackMigration { filename: "knex:last".into() }))
    }
}
