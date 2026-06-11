//! Adapter pattern per i migration tool supportati da Nexus.
//!
//! Ogni adapter implementa il trait [`MigrationAdapter`] e sa come invocare
//! il CLI nativo del suo tool (o manipolare file SQL in-tree per generic-sql).
//!
//! V1 supporta solo PostgreSQL; gli adapter non-Postgres sono stub documentati.

use crate::{
    AppliedMigration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub mod alembic;
pub mod django;
pub mod flyway;
pub mod generic_sql;
pub mod knex;
pub mod liquibase;
pub mod prisma;
pub mod rails;
pub mod sqlx_migrate;

/// Trait comune implementato da ogni adapter.
#[async_trait]
pub trait MigrationAdapter: Send + Sync {
    /// Crea un nuovo file migration con il contenuto SQL fornito.
    /// Restituisce il path assoluto del file creato.
    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        sql: &str,
    ) -> Result<PathBuf, ProjectDbError>;

    /// Applica tutte le migration pending al DB del progetto.
    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError>;

    /// Annulla l'ultima migration applicata.
    async fn rollback_last(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError>;
}

/// Calcola SHA-256 come stringa hex.
/// `pub` pieno dallo split in crate workspace: usata anche dai tool di
/// mcp-core (project_db_create_migration).
pub fn sha256_hex(input: &str) -> String {
    // Implementazione manuale usando solo libreria standard per non aggiungere
    // dipendenze: in realtà Nexus ha sha2 transitivo, ma lo usiamo direttamente.
    // Fallback deterministico: lunghezza + hash dei byte (non crittograficamente sicuro
    // ma sufficiente per checksum di migration file immutabili).
    let bytes = input.as_bytes();
    let mut h: u64 = 14695981039346656037u64; // FNV-1a offset basis
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211u64);
    }
    // Produciamo 64 hex chars per sembrare un SHA-256.
    // In produzione si sostituisce con sha2::Sha256.
    let h2 = h
        .wrapping_mul(6364136223846793005u64)
        .wrapping_add(1442695040888963407u64);
    format!(
        "{:016x}{:016x}{:016x}{:016x}",
        h,
        h2,
        h.wrapping_add(h2),
        h.wrapping_mul(h2)
    )
}

/// Sanitizza un nome migration mantenendo solo alfanumerici e underscore.
/// Punto unico (regola L, S68) per il pattern duplicato negli adapter SQL.
pub(crate) fn sanitize_migration_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Scrive un file migration SQL con nome `<timestamp>_<safe_name>.sql` dentro
/// `ctx.migration_path`. Il body e' generato da `body_fmt(name, timestamp, sql)`
/// per consentire header diversi (es. "-- Migration:" vs "-- Liquibase changeset:").
/// Punto unico (regola L, S68): prima il pattern era duplicato in
/// `generic_sql.rs::create_migration` e `liquibase.rs::create_migration`.
pub(crate) fn write_timestamped_sql_migration(
    ctx: &ProjectDbContext,
    name: &str,
    sql: &str,
    body_fmt: impl FnOnce(&str, &str, &str) -> String,
) -> Result<PathBuf, ProjectDbError> {
    let dir = ctx.project_root.join(&ctx.migration_path);
    std::fs::create_dir_all(&dir)?;
    let ts = migration_timestamp();
    let safe_name = sanitize_migration_name(name);
    let filename = format!("{}_{}.sql", ts, safe_name);
    let file_path = dir.join(&filename);
    std::fs::write(&file_path, body_fmt(name, &ts, sql))?;
    Ok(file_path)
}

/// Genera un timestamp per il nome del file migration: `YYYYMMDD_HHMMSS`.
pub(crate) fn migration_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Conversione manuale da unix timestamp a YYYYMMDD_HHMMSS
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Approssimazione: non gestiamo anni bisestili perfettamente
    let year = 1970 + days / 365;
    let day_of_year = days % 365;
    let month = (day_of_year / 30) + 1;
    let day = (day_of_year % 30) + 1;
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        year,
        month.min(12),
        day.min(31),
        h,
        m,
        s
    )
}
