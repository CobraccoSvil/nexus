//! Adapter `generic-sql` — gestione migration tramite file SQL plain.
//!
//! Usato quando non viene rilevato nessun tool specifico ma esiste una
//! cartella `migrations/` con file `.sql`. Nexus numera i file con il
//! pattern `YYYYMMDD_HHMMSS_<nome>.sql` e li applica in ordine.

use super::MigrationAdapter;
use crate::project_db::{
    AppliedMigration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct GenericSqlAdapter;

#[async_trait]
impl MigrationAdapter for GenericSqlAdapter {
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
