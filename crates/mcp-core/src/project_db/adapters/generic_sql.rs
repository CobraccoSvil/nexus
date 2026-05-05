//! Adapter `generic-sql` — gestione migration tramite file SQL plain.
//!
//! Usato quando non viene rilevato nessun tool specifico ma esiste una
//! cartella `migrations/` con file `.sql`. Nexus numera i file con il
//! pattern `YYYYMMDD_HHMMSS_<nome>.sql` e li applica in ordine.

use async_trait::async_trait;
use std::path::PathBuf;
use crate::project_db::{Migration, AppliedMigration, RolledBackMigration, ProjectDbError, ProjectDbContext};
use super::{MigrationAdapter, migration_timestamp, sha256_hex};

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
            let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
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
        let dir = ctx.project_root.join(&ctx.migration_path);
        std::fs::create_dir_all(&dir)?;

        let ts = migration_timestamp();
        let safe_name: String = name.chars()
            .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
            .collect();
        let filename = format!("{}_{}.sql", ts, safe_name);
        let file_path = dir.join(&filename);

        let header = format!("-- Migration: {}\n-- Creata da Nexus il {}\n\n", name, ts);
        std::fs::write(&file_path, format!("{}{}", header, sql))?;
        Ok(file_path)
    }

    async fn apply_pending(
        &self,
        _ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        // V1: l'applicazione avviene via sqlx direttamente dal runner.
        // Questo stub segnala che il runner deve gestire l'esecuzione SQL raw.
        Err(ProjectDbError::Adapter(
            "apply tramite runner.rs per generic-sql".into()
        ))
    }

    async fn rollback_last(
        &self,
        _ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        Err(ProjectDbError::Adapter(
            "rollback non supportato in generic-sql senza rollback_sql".into()
        ))
    }
}
