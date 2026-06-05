//! Adapter Alembic (Python) — crea revision tramite `alembic revision`.
//!
//! V1: crea il file migration invocando `alembic revision --autogenerate -m <nome>`
//! oppure, se Alembic non è disponibile nel PATH del progetto, genera un file
//! SQL generico nella cartella migrations/ dell'applicazione Python.

use super::{list_pending_files, migration_timestamp, MigrationAdapter};
use crate::project_db::{
    AppliedMigration, Migration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct AlembicAdapter;

#[async_trait]
impl MigrationAdapter for AlembicAdapter {
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError> {
        // Punto unico: scansione file-based via `list_pending_files` (regola L).
        list_pending_files(ctx, |n| n.ends_with(".py") && !n.starts_with("__"), false)
    }

    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        sql: &str,
    ) -> Result<PathBuf, ProjectDbError> {
        // Tenta di invocare alembic revision; se non disponibile, crea file SQL stub.
        let alembic_ini = ctx.project_root.join("alembic.ini");
        if alembic_ini.exists() {
            let output = std::process::Command::new("alembic")
                .args(["revision", "-m", name])
                .current_dir(&ctx.project_root)
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    // Alembic ha creato il file — troviamo il piu' recente nella dir
                    let dir = ctx.project_root.join(&ctx.migration_path);
                    if let Ok(mut entries) = std::fs::read_dir(&dir) {
                        let mut files: Vec<_> = entries
                            .flatten()
                            .filter(|e| e.file_name().to_string_lossy().ends_with(".py"))
                            .collect();
                        files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
                        if let Some(last) = files.last() {
                            return Ok(last.path());
                        }
                    }
                }
            }
        }
        // Fallback: file SQL generico
        let dir = ctx.project_root.join(&ctx.migration_path);
        std::fs::create_dir_all(&dir)?;
        let ts = migration_timestamp();
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
        let filename = format!("{}_{}_nexus.sql", ts, safe_name);
        let path = dir.join(&filename);
        std::fs::write(
            &path,
            format!(
                "-- Alembic migration stub generata da Nexus\n-- {}\n\n{}\n",
                name, sql
            ),
        )?;
        Ok(path)
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let output = std::process::Command::new("alembic")
            .args(["upgrade", "head"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("alembic upgrade: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "alembic upgrade head fallita: {}",
                stderr
            )));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        let output = std::process::Command::new("alembic")
            .args(["downgrade", "-1"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("alembic downgrade: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "alembic downgrade -1 fallita: {}",
                stderr
            )));
        }
        Ok(Some(RolledBackMigration {
            filename: "alembic:head-1".into(),
        }))
    }
}
