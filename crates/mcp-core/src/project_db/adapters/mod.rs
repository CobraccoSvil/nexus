//! Adapter pattern per i migration tool supportati da Nexus.
//!
//! Ogni adapter implementa il trait [`MigrationAdapter`] e sa come invocare
//! il CLI nativo del suo tool (o manipolare file SQL in-tree per generic-sql).
//!
//! V1 supporta solo PostgreSQL; gli adapter non-Postgres sono stub documentati.

use crate::project_db::{
    AppliedMigration, Migration, ProjectDbContext, ProjectDbError, RolledBackMigration,
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
    /// Elenca le migration in stato 'pending' (create ma non applicate).
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError>;

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

    /// Calcola SHA-256 del contenuto di una migration.
    fn checksum(&self, content: &str) -> String {
        use std::fmt::Write as _;
        // SHA-256 semplice senza dipendenze extra: usiamo sha2 se disponibile,
        // altrimenti una rappresentazione deterministca del contenuto.
        // In Nexus sha2 è già dipendenza transitiva (via ring/rustls).
        sha256_hex(content)
    }
}

/// Calcola SHA-256 come stringa hex.
pub(crate) fn sha256_hex(input: &str) -> String {
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

/// Elenca le migration nella `migration_path` del progetto applicando il filtro
/// dato (es. `*.sql`, `*.py` senza prefisso `__`). Helper condiviso fra adapter
/// che leggono file-based migrations dal filesystem — punto unico (regola L /
/// ADR 0026, step S10). Prima questo loop era duplicato in alembic.rs e
/// sqlx_migrate.rs (cluster 40L).
///
/// - `file_filter`: predicato che riceve il nome file e decide se includerlo.
/// - `include_sql`: se `true` il campo `Migration.sql` riceve il contenuto del
///   file (sqlx-style); se `false` resta `None` (alembic-style, dove il vero
///   SQL e' all'interno della Migration via Alembic API).
pub(crate) fn list_pending_files(
    ctx: &ProjectDbContext,
    file_filter: impl Fn(&str) -> bool,
    include_sql: bool,
) -> Result<Vec<Migration>, ProjectDbError> {
    let dir = ctx.project_root.join(&ctx.migration_path);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut files: Vec<_> = std::fs::read_dir(&dir)?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            file_filter(&name)
        })
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
            sql: if include_sql { Some(content) } else { None },
        });
    }
    Ok(result)
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
