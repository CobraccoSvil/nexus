//! `runner` — orchestratore migration: delega all'adapter del progetto,
//! blocca DDL diretto e gestisce il guardrail con errore strutturato.

use std::sync::Arc;
use uuid::Uuid;
use crate::project_db::{
    adapters::{
        MigrationAdapter,
        alembic::AlembicAdapter,
        django::DjangoAdapter,
        flyway::FlywayAdapter,
        generic_sql::GenericSqlAdapter,
        knex::KnexAdapter,
        liquibase::LiquibaseAdapter,
        prisma::PrismaAdapter,
        sqlx_migrate::SqlxMigrateAdapter,
    },
    AppliedMigration, Migration, MigrationTool, ProjectDbContext, ProjectDbError, RolledBackMigration,
};

/// Parole chiave DDL da bloccare quando il target è un progetto utente.
const DDL_KEYWORDS: &[&str] = &[
    "CREATE TABLE", "CREATE INDEX", "CREATE VIEW", "CREATE SEQUENCE",
    "CREATE TYPE", "CREATE FUNCTION", "CREATE TRIGGER", "CREATE SCHEMA",
    "ALTER TABLE", "ALTER COLUMN", "ALTER INDEX",
    "DROP TABLE", "DROP INDEX", "DROP VIEW", "DROP COLUMN",
    "DROP SCHEMA", "DROP SEQUENCE", "DROP TYPE", "DROP FUNCTION",
    "DROP TRIGGER",
    "TRUNCATE", "RENAME TABLE", "RENAME COLUMN",
];

/// Verifica se il SQL contiene istruzioni DDL.
pub fn contains_ddl(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    DDL_KEYWORDS.iter().any(|kw| upper.contains(kw))
}

/// Errore strutturato restituito quando DDL viene bloccato.
#[derive(Debug, serde::Serialize)]
pub struct DdlBlockedPayload {
    pub error: &'static str,
    pub message: String,
    pub suggested_tool: &'static str,
    pub override_endpoint: String,
}

impl DdlBlockedPayload {
    pub fn new(project_id: Uuid) -> Self {
        Self {
            error: "DDL_BLOCKED",
            message: "Modifica schema bloccata. Usa project_db_create_migration per creare una migration tracciabile.".into(),
            suggested_tool: "project_db_create_migration",
            override_endpoint: format!("/api/projects/{}/db/override-request", project_id),
        }
    }
}

/// Seleziona l'adapter corretto in base al tool dichiarato.
pub fn adapter_for(tool: &MigrationTool) -> Arc<dyn MigrationAdapter> {
    match tool {
        MigrationTool::Alembic => Arc::new(AlembicAdapter),
        MigrationTool::Prisma => Arc::new(PrismaAdapter),
        MigrationTool::Sqlx => Arc::new(SqlxMigrateAdapter),
        MigrationTool::Flyway => Arc::new(FlywayAdapter),
        MigrationTool::Django => Arc::new(DjangoAdapter),
        MigrationTool::Rails => Arc::new(RailsAdapter),
        MigrationTool::Knex => Arc::new(KnexAdapter),
        MigrationTool::Liquibase => Arc::new(LiquibaseAdapter),
        MigrationTool::GenericSql => Arc::new(GenericSqlAdapter),
    }
}

// Import locale per Rails
use crate::project_db::adapters::rails::RailsAdapter;

/// Orchestratore principale — usato dai 4 tool MCP `project_db_*`.
pub struct MigrationRunner {
    adapter: Arc<dyn MigrationAdapter>,
    ctx: ProjectDbContext,
}

impl MigrationRunner {
    pub fn new(ctx: ProjectDbContext) -> Self {
        let adapter = adapter_for(&ctx.migration_tool);
        Self { adapter, ctx }
    }

    /// Lista migration pending.
    pub async fn list_pending(&self) -> Result<Vec<Migration>, ProjectDbError> {
        self.adapter.list_pending(&self.ctx).await
    }

    /// Crea un file migration per il SQL fornito.
    /// Blocca DDL diretto se `check_ddl` è true.
    pub async fn create_migration(
        &self,
        name: &str,
        sql: &str,
        check_ddl: bool,
    ) -> Result<std::path::PathBuf, ProjectDbError> {
        if check_ddl && contains_ddl(sql) {
            return Err(ProjectDbError::DdlBlocked {
                suggested_tool: "project_db_create_migration".into(),
            });
        }
        self.adapter.create_migration(&self.ctx, name, sql).await
    }

    /// Applica tutte le migration pending al DB del progetto.
    pub async fn apply_pending(&self, connection_url: &str) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        self.adapter.apply_pending(&self.ctx, connection_url).await
    }

    /// Annulla l'ultima migration.
    pub async fn rollback_last(&self, connection_url: &str) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        self.adapter.rollback_last(&self.ctx, connection_url).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ddl_detection_create_table() {
        assert!(contains_ddl("CREATE TABLE users (id SERIAL PRIMARY KEY)"));
    }

    #[test]
    fn test_ddl_detection_alter() {
        assert!(contains_ddl("ALTER TABLE orders ADD COLUMN status TEXT"));
    }

    #[test]
    fn test_ddl_detection_select_no_block() {
        assert!(!contains_ddl("SELECT * FROM users WHERE id = $1"));
    }

    #[test]
    fn test_ddl_detection_insert_no_block() {
        assert!(!contains_ddl("INSERT INTO events (name) VALUES ('test')"));
    }

    #[test]
    fn test_ddl_detection_drop() {
        assert!(contains_ddl("drop table legacy_data"));
    }

    #[test]
    fn test_ddl_detection_truncate() {
        assert!(contains_ddl("TRUNCATE TABLE temp_cache"));
    }
}
