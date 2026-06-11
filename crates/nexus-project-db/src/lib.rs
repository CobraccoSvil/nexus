//! `project_db` — gestione database e migrazioni per i **progetti utente** in Nexus.
//!
//! Questo modulo NON tocca il database interno di Nexus: opera esclusivamente
//! sul DB dichiarato da ciascun progetto importato via `ProjectImportWizard`.
//!
//! ## Sottosistemi
//!
//! - [`detector`] — rileva motore DB e migration tool dal filesystem del progetto.
//! - [`runner`] — orchestratore che delega all'adapter appropriato.
//! - [`adapters`] — un adapter per ogni migration tool supportato.
//!
//! ## Flusso tipico
//!
//! 1. `detector::detect_db_profile(project_path)` → `DbProfile`
//! 2. Il profilo viene salvato in `project_database_config`.
//! 3. Ogni tool MCP che tocca lo schema passa per `runner::MigrationRunner`.
//! 4. DDL diretto viene bloccato con `DdlBlockedError`; il runner genera file migration.

pub mod adapters;
pub mod detector;
pub mod exec;
pub mod runner;

use std::path::PathBuf;

// ── Errore unificato ──────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ProjectDbError {
    #[error("DDL bloccato: usa project_db_create_migration. Tool suggerito: {suggested_tool}")]
    DdlBlocked { suggested_tool: String },

    #[error("Errore filesystem: {0}")]
    Io(#[from] std::io::Error),

    #[error("Errore serializzazione: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Adapter error: {0}")]
    Adapter(String),
}

// ── Tipi pubblici ─────────────────────────────────────────────────────────

/// Motore DB rilevato o configurato per un progetto utente.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbEngine {
    Postgres,
    Mysql,
    Sqlite,
    Mongodb,
    Sqlserver,
    Unknown(String),
}

impl DbEngine {
    pub fn as_str(&self) -> &str {
        match self {
            DbEngine::Postgres => "postgres",
            DbEngine::Mysql => "mysql",
            DbEngine::Sqlite => "sqlite",
            DbEngine::Mongodb => "mongodb",
            DbEngine::Sqlserver => "sqlserver",
            DbEngine::Unknown(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for DbEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Migration tool rilevato o scelto per un progetto utente.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationTool {
    Alembic,
    Prisma,
    Sqlx,
    Flyway,
    Django,
    Rails,
    Knex,
    Liquibase,
    GenericSql,
}

impl MigrationTool {
    pub fn as_str(&self) -> &str {
        match self {
            MigrationTool::Alembic => "alembic",
            MigrationTool::Prisma => "prisma",
            MigrationTool::Sqlx => "sqlx",
            MigrationTool::Flyway => "flyway",
            MigrationTool::Django => "django",
            MigrationTool::Rails => "rails",
            MigrationTool::Knex => "knex",
            MigrationTool::Liquibase => "liquibase",
            MigrationTool::GenericSql => "generic-sql",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "alembic" => Some(MigrationTool::Alembic),
            "prisma" => Some(MigrationTool::Prisma),
            "sqlx" => Some(MigrationTool::Sqlx),
            "flyway" => Some(MigrationTool::Flyway),
            "django" => Some(MigrationTool::Django),
            "rails" => Some(MigrationTool::Rails),
            "knex" => Some(MigrationTool::Knex),
            "liquibase" => Some(MigrationTool::Liquibase),
            "generic-sql" => Some(MigrationTool::GenericSql),
            _ => None,
        }
    }
}

impl std::fmt::Display for MigrationTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Profilo DB rilevato dal detector per un progetto utente.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DbProfile {
    pub engine: DbEngine,
    pub migration_tool: Option<MigrationTool>,
    /// Path relativo alla cartella migrations dentro il progetto.
    pub migration_path: Option<String>,
    /// File marker che ha determinato il rilevamento (per audit/UI).
    pub marker_files: Vec<String>,
    /// Confidenza del rilevamento (0.0 - 1.0).
    pub confidence: f32,
}

/// Risultato dell'applicazione di una migration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppliedMigration {
    pub filename: String,
    pub checksum: String,
}

/// Risultato del rollback di una migration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RolledBackMigration {
    pub filename: String,
}

/// Contesto passato al runner e agli adapter.
#[derive(Debug, Clone)]
pub struct ProjectDbContext {
    /// Path assoluta della root del progetto utente.
    pub project_root: PathBuf,
    /// Tool di migrazione configurato.
    pub migration_tool: MigrationTool,
    /// Path relativa alla cartella migrations.
    pub migration_path: String,
}
