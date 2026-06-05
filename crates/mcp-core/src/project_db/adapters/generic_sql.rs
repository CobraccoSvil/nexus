//! Adapter `generic-sql` — gestione migration tramite file SQL plain.
//!
//! Usato quando non viene rilevato nessun tool specifico ma esiste una
//! cartella `migrations/` con file `.sql`. Nexus numera i file con il
//! pattern `YYYYMMDD_HHMMSS_<nome>.sql` e li applica in ordine.

use super::{migration_timestamp, sha256_hex, MigrationAdapter};
use crate::project_db::{
    AppliedMigration, Migration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct GenericSqlAdapter;

#[async_trait]
impl MigrationAdapter for GenericSqlAdapter {
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
            let content = std::fs::read_to_string(&path)?;
            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            result.push(Migration {
                filename: filename.clone(),
                checksum: sha256_hex(&content),
                description: Some(filename.trim_end_matches(".sql").to_string()),
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
        // Punto unico in super::write_timestamped_sql_migration (regola L, S68).
        super::write_timestamped_sql_migration(ctx, name, sql, |n, ts, body| {
            format!(
                "-- Migration: {}\n-- Creata da Nexus il {}\n\n{}",
                n, ts, body
            )
        })
    }

    async fn apply_pending(
        &self,
        _ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        // V1: l'applicazione avviene via sqlx direttamente dal runner.
        // Questo stub segnala che il runner deve gestire l'esecuzione SQL raw.
        Err(ProjectDbError::Adapter(
            "apply tramite runner.rs per generic-sql".into(),
        ))
    }

    async fn rollback_last(
        &self,
        _ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        Err(ProjectDbError::Adapter(
            "rollback non supportato in generic-sql senza rollback_sql".into(),
        ))
    }
}
