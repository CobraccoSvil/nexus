//! Registrazione handler dominio: database
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        db_active_queries::DbActiveQueriesTool, db_bloat_check::DbBloatCheckTool,
        db_connection_info::DbConnectionInfoTool, db_constraint_list::DbConstraintListTool,
        db_dead_tuples::DbDeadTuplesTool, db_extension_list::DbExtensionListTool,
        db_foreign_keys::DbForeignKeysTool, db_index_list::DbIndexListTool,
        db_lock_list::DbLockListTool, db_migration_list::DbMigrationListTool, db_ping::DbPingTool,
        db_query_explain::DbQueryExplainTool, db_replication_status::DbReplicationStatusTool,
        db_role_list::DbRoleListTool, db_schema_inspect::DbSchemaInspectTool,
        db_seq_list::DbSeqListTool, db_size::DbSizeTool, db_table_count::DbTableCountTool,
        db_table_list::DbTableListTool, db_table_size::DbTableSizeTool,
        db_unused_indexes::DbUnusedIndexesTool, db_view_list::DbViewListTool,
        project_db_analyze::ProjectDbAnalyzeTool,
        project_db_apply_migration::ProjectDbApplyMigrationTool,
        project_db_backup::ProjectDbBackupTool, project_db_connections::ProjectDbConnectionsTool,
        project_db_create_migration::ProjectDbCreateMigrationTool,
        project_db_diff_schema::ProjectDbDiffSchemaTool,
        project_db_dump_schema::ProjectDbDumpSchemaTool,
        project_db_kill_query::ProjectDbKillQueryTool, project_db_query::ProjectDbQueryTool,
        project_db_reindex::ProjectDbReindexTool, project_db_restore::ProjectDbRestoreTool,
        project_db_rollback::ProjectDbRollbackTool, project_db_schema::ProjectDbSchemaTool,
        project_db_set_connection::ProjectDbSetConnectionTool,
        project_db_status::ProjectDbStatusTool, project_db_tables::ProjectDbTablesTool,
        project_db_vacuum::ProjectDbVacuumTool,
    };

    // Database (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "db_schema_inspect",
            NexusToolCategory::Database,
            "Inspect PostgreSQL schema via information_schema",
        ),
        Arc::new(DbSchemaInspectTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_query_explain",
            NexusToolCategory::Database,
            "EXPLAIN (VERBOSE, FORMAT JSON) for SELECT/WITH queries",
        ),
        Arc::new(DbQueryExplainTool),
    );

    // Database extras (Fase 9K, 20 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "db_ping",
            NexusToolCategory::Database,
            "SELECT 1 connectivity test against DATABASE_URL",
        ),
        Arc::new(DbPingTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_table_list",
            NexusToolCategory::Database,
            "List tables in a schema (default public)",
        ),
        Arc::new(DbTableListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_table_count",
            NexusToolCategory::Database,
            "SELECT COUNT(*) for a specific table",
        ),
        Arc::new(DbTableCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_index_list",
            NexusToolCategory::Database,
            "List indexes in a schema from pg_indexes",
        ),
        Arc::new(DbIndexListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_view_list",
            NexusToolCategory::Database,
            "List views in a schema from pg_views",
        ),
        Arc::new(DbViewListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_role_list",
            NexusToolCategory::Database,
            "List roles from pg_roles",
        ),
        Arc::new(DbRoleListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_extension_list",
            NexusToolCategory::Database,
            "List installed extensions from pg_extension",
        ),
        Arc::new(DbExtensionListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_size",
            NexusToolCategory::Database,
            "Total size of the current database (pg_database_size)",
        ),
        Arc::new(DbSizeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_connection_info",
            NexusToolCategory::Database,
            "Current connection info (user, db, host, version)",
        ),
        Arc::new(DbConnectionInfoTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_migration_list",
            NexusToolCategory::Database,
            "List .sql migration files under db/migrations or migrations",
        ),
        Arc::new(DbMigrationListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_seq_list",
            NexusToolCategory::Database,
            "List sequences in a schema",
        ),
        Arc::new(DbSeqListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_foreign_keys",
            NexusToolCategory::Database,
            "List foreign keys in a schema with referenced table/column",
        ),
        Arc::new(DbForeignKeysTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_unused_indexes",
            NexusToolCategory::Database,
            "Indexes never scanned (idx_scan = 0) from pg_stat_user_indexes",
        ),
        Arc::new(DbUnusedIndexesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_dead_tuples",
            NexusToolCategory::Database,
            "Top tables by dead tuples from pg_stat_user_tables",
        ),
        Arc::new(DbDeadTuplesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_bloat_check",
            NexusToolCategory::Database,
            "Quick bloat estimate via dead/live ratio",
        ),
        Arc::new(DbBloatCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_table_size",
            NexusToolCategory::Database,
            "Total + heap size for a specific table",
        ),
        Arc::new(DbTableSizeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_constraint_list",
            NexusToolCategory::Database,
            "List constraints in a schema with type",
        ),
        Arc::new(DbConstraintListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_lock_list",
            NexusToolCategory::Database,
            "Active locks from pg_locks joined with pg_stat_activity",
        ),
        Arc::new(DbLockListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_active_queries",
            NexusToolCategory::Database,
            "Non-idle queries from pg_stat_activity",
        ),
        Arc::new(DbActiveQueriesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "db_replication_status",
            NexusToolCategory::Database,
            "Replication status from pg_stat_replication",
        ),
        Arc::new(DbReplicationStatusTool),
    );

    // Project DB tools — gestione DB e migration per progetti utente
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_connections",
            NexusToolCategory::Database,
            "Restituisce le connessioni DB configurate per il progetto corrente (connection string, engine, ecc.)",
        ),
        Arc::new(ProjectDbConnectionsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_status",
            NexusToolCategory::Database,
            "Stato DB e migration del progetto utente corrente (read-only)",
        ),
        Arc::new(ProjectDbStatusTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_create_migration",
            NexusToolCategory::Database,
            "Crea file migration timestampato per il DB del progetto. Blocca DDL diretto.",
        ),
        Arc::new(ProjectDbCreateMigrationTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_apply_migration",
            NexusToolCategory::Database,
            "Applica migration pending al DB del progetto utente.",
        ),
        Arc::new(ProjectDbApplyMigrationTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_rollback",
            NexusToolCategory::Database,
            "Annulla l'ultima migration applicata al DB del progetto utente.",
        ),
        Arc::new(ProjectDbRollbackTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_set_connection",
            NexusToolCategory::Database,
            "Configura la connessione al DB del progetto. Parametri: connection_string (DSN PostgreSQL), engine (postgres), hosting_mode (internal/external).",
        ),
        Arc::new(ProjectDbSetConnectionTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_query",
            NexusToolCategory::Database,
            "Esegue una query read-only (SELECT/WITH/EXPLAIN/SHOW) sul DB del progetto corrente. NON usare psql. DDL/DML scrittura sono rifiutati. Limit 100 righe.",
        ),
        Arc::new(ProjectDbQueryTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_schema",
            NexusToolCategory::Database,
            "Ispeziona lo schema del DB del progetto corrente: tabelle, colonne, tipi, nullable, default. Filtra per schema (default 'public') o singola tabella.",
        ),
        Arc::new(ProjectDbSchemaTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_tables",
            NexusToolCategory::Database,
            "Lista sintetica delle tabelle del DB del progetto corrente: nome, stima righe, dimensione. Piu' veloce di project_db_schema.",
        ),
        Arc::new(ProjectDbTablesTool),
    );

    // Fase 6: Operazioni DB avanzate
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_backup",
            NexusToolCategory::Database,
            "Esegue pg_dump sul DB del progetto. Salva in .nexus/backups/. Supporta formato plain/custom, schema-only.",
        ),
        Arc::new(ProjectDbBackupTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_restore",
            NexusToolCategory::Database,
            "Ripristina un backup nel DB del progetto. Richiede confirm:true. Supporta plain SQL e formato custom.",
        ),
        Arc::new(ProjectDbRestoreTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_vacuum",
            NexusToolCategory::Database,
            "Esegue VACUUM sul DB del progetto. Supporta ANALYZE e FULL. Opera su tabella singola o intero database.",
        ),
        Arc::new(ProjectDbVacuumTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_analyze",
            NexusToolCategory::Database,
            "Esegue ANALYZE sul DB del progetto. Aggiorna le statistiche del query planner.",
        ),
        Arc::new(ProjectDbAnalyzeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_reindex",
            NexusToolCategory::Database,
            "Esegue REINDEX su tabella/indice del DB progetto. Operazione bloccante.",
        ),
        Arc::new(ProjectDbReindexTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_dump_schema",
            NexusToolCategory::Database,
            "Esporta solo lo schema del DB progetto (pg_dump --schema-only). Snapshot pre-migration.",
        ),
        Arc::new(ProjectDbDumpSchemaTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_diff_schema",
            NexusToolCategory::Database,
            "Confronta lo schema DB attuale con un file SQL di riferimento. Utile per verifica post-migration.",
        ),
        Arc::new(ProjectDbDiffSchemaTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "project_db_kill_query",
            NexusToolCategory::Database,
            "Termina una query bloccante sul DB progetto. Usa pg_cancel_backend (graceful) o pg_terminate_backend (force).",
        ),
        Arc::new(ProjectDbKillQueryTool),
    );
}
