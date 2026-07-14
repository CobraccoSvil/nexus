//! Gestori HTTP per le API project-DB.
//!
//! Package suddiviso per responsabilita' (audit best-practice, regola H):
//!   - `shared`      : tipi/helper condivisi (ApiResult, normalize/target DSN)
//!   - `config`      : get/set config + list/set-primary/delete connessioni
//!   - `provision`   : provisioning internal/external + core riusabile
//!   - `connection`  : test-connection + detection automatica + SQL Server
//!   - `migrations`  : list/apply/rollback + request DDL override
//!   - `query`       : execute query client SQL + import schema
//!
//! Route montate in `routes/project_db.rs`:
//!   GET    /api/projects/:id/db                                   -> get_project_db_config
//!   POST   /api/projects/:id/db/config                           -> set_project_db_config
//!   POST   /api/projects/:id/db/provision                        -> provision_project_db
//!   GET    /api/projects/:id/db/connections                      -> list_project_db_connections
//!   POST   /api/projects/:id/db/connections/:conn_id/set-primary -> set_primary_project_db_connection
//!   DELETE /api/projects/:id/db/connections/:conn_id             -> delete_project_db_connection
//!   GET    /api/projects/:id/db/migrations                       -> list_project_migrations
//!   POST   /api/projects/:id/db/migrations/apply                 -> apply_project_migrations
//!   POST   /api/projects/:id/db/migrations/rollback              -> rollback_project_migration
//!   POST   /api/projects/:id/db/override-request                 -> request_ddl_override
//!   POST   /api/projects/:id/db/detect                           -> detect_project_db
//!   POST   /api/projects/:id/db/test-connection                  -> test_project_db_connection
//!   POST   /api/projects/:id/db/query                            -> execute_project_db_query
//!   POST   /api/projects/:id/db/import-schema                    -> import_project_db_schema

mod config;
mod connection;
mod migrations;
mod provision;
mod query;
mod shared;

// Ri-esporta il contratto pubblico invariato (stessi simboli del modulo
// monolitico precedente). I call site esterni (`routes/project_db.rs`,
// `nexus_builtin/mod.rs`) non cambiano.
pub use config::{
    delete_project_db_connection, get_project_db_config, list_project_db_connections,
    set_primary_project_db_connection, set_project_db_config,
};
pub use connection::{detect_project_db, test_project_db_connection};

// Punto unico (regola L) per estrarre il bersaglio fisico (host, port, dbname) da
// una connection string / DSN postgres. Riusato dal cleanup di cancellazione
// progetto (`projects::cleanup::drop_internal_app_databases`) per applicare i
// guard di sicurezza sul nome del database prima del DROP.
pub use migrations::{
    apply_project_migrations, list_project_migrations, request_ddl_override,
    rollback_project_migration,
};
// Punto unico (regola L) della derivazione del nome DB fisico del progetto e
// della lettura dei settings del cluster app: vi delega
// `agent_tools::command::ensure_project_db_url`, che ne teneva copie divergenti.
pub(crate) use provision::{derive_project_db_name, load_app_db_setting};
pub use provision::{
    init_global_pools, list_all_project_ids, project_data_pool,
    project_data_pool_by_correction_from, project_data_pool_by_feedback_from,
    project_data_pool_by_message_from, project_data_pool_by_run_from, project_data_pool_by_session,
    project_data_pool_by_session_from, project_data_pool_from, provision_internal_core,
    provision_project_db, register_entity_routing, DbRole,
};
pub(crate) use shared::pg_physical_target;
// Esportati come fondazione; non ancora richiamati da call-site interni.
#[allow(unused_imports)]
pub use provision::project_meta_pool;
pub use query::{
    discover_schema_candidates, execute_project_db_query, import_project_db_schema,
    read_schema_file,
};
