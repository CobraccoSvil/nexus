//! Adapter Prisma (Node.js/TypeScript) — crea migration tramite `prisma migrate dev --create-only`.

use super::MigrationAdapter;
use crate::{
    AppliedMigration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct PrismaAdapter;

#[async_trait]
impl MigrationAdapter for PrismaAdapter {
    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        _sql: &str,
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
        let output = std::process::Command::new("npx")
            .args([
                "prisma",
                "migrate",
                "dev",
                "--create-only",
                "--name",
                &safe_name,
            ])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("prisma migrate dev: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "prisma migrate dev fallita: {}",
                stderr
            )));
        }
        // Trova la directory migration appena creata
        let base = ctx.project_root.join(&ctx.migration_path);
        let mut dirs: Vec<_> = std::fs::read_dir(&base)
            .map_err(ProjectDbError::Io)?
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
            return Err(ProjectDbError::Adapter(format!(
                "prisma migrate deploy fallita: {}",
                stderr
            )));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        _ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        Err(ProjectDbError::Adapter(
            "Prisma non supporta rollback diretto. Usa reset o crea una migration inversa.".into(),
        ))
    }
}
