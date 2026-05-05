//! Adapter Prisma (Node.js/TypeScript) — crea migration tramite `prisma migrate dev --create-only`.

use async_trait::async_trait;
use std::path::PathBuf;
use crate::project_db::{Migration, AppliedMigration, RolledBackMigration, ProjectDbError, ProjectDbContext};
use super::{MigrationAdapter, sha256_hex};

pub struct PrismaAdapter;

#[async_trait]
impl MigrationAdapter for PrismaAdapter {
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError> {
        // Prisma migrations: directory con migration.sql dentro
        let base = ctx.project_root.join(&ctx.migration_path);
        if !base.exists() { return Ok(vec![]); }
        let mut result = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&base) {
            let mut dirs: Vec<_> = entries.flatten()
                .filter(|e| e.path().is_dir())
                .collect();
            dirs.sort_by_key(|e| e.file_name());
            for dir in dirs {
                let sql_path = dir.path().join("migration.sql");
                if sql_path.exists() {
                    let content = std::fs::read_to_string(&sql_path).unwrap_or_default();
                    let dirname = dir.file_name().to_string_lossy().to_string();
                    result.push(Migration {
                        filename: format!("{}/migration.sql", dirname),
                        checksum: sha256_hex(&content),
                        description: Some(dirname),
                        sql: Some(content),
                    });
                }
            }
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
            .args(["prisma", "migrate", "dev", "--create-only", "--name", &safe_name])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("prisma migrate dev: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!("prisma migrate dev fallita: {}", stderr)));
        }
        // Trova la directory migration appena creata
        let base = ctx.project_root.join(&ctx.migration_path);
        let mut dirs: Vec<_> = std::fs::read_dir(&base)
            .map_err(|e| ProjectDbError::Io(e))?
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        dirs.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
        dirs.last()
            .map(|d| d.path().join("migration.sql"))
            .ok_or_else(|| ProjectDbError::Adapter("directory migration Prisma non trovata".into()))
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let output = std::process::Command::new("npx")
            .args(["prisma", "migrate", "deploy"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("prisma migrate deploy: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!("prisma migrate deploy fallita: {}", stderr)));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        _ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        Err(ProjectDbError::Adapter(
            "Prisma non supporta rollback diretto. Usa reset o crea una migration inversa.".into()
        ))
    }
}
