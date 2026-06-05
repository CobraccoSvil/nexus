//! Helper condivisi per i tool `project_db_*` (regola L / ADR 0026, step S4).
//!
//! Prima `get_project_dsn` era duplicata pari-pari in 4 file:
//! - project_db_backup.rs
//! - project_db_diff_schema.rs
//! - project_db_dump_schema.rs
//! - project_db_restore.rs
//!
//! Ognuna era ~36 righe (cluster jscpd 58L+51L+34L). Ora vive qui una volta sola.

use crate::nexus_tools::{db_helper, NexusToolError};

/// Risolve il DSN Postgres del progetto leggendolo da `project_database_config`.
/// Ritorna l'errore se: il progetto non ha config, l'engine non e' 'postgres',
/// il `connection_secret` non e' UTF-8, o il DSN non e' normalizzabile.
pub async fn get_project_dsn(
    nexus_pool: &sqlx::PgPool,
    project_id: uuid::Uuid,
) -> Result<String, NexusToolError> {
    let row: Option<(Vec<u8>, String)> = sqlx::query_as(
        r#"SELECT connection_secret, engine
           FROM project_database_config
           WHERE project_id = $1
           ORDER BY is_primary DESC, created_at ASC
           LIMIT 1"#,
    )
    .bind(project_id)
    .fetch_optional(nexus_pool)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("lookup config: {}", e)))?;

    let (secret_bytes, engine) = row.ok_or_else(|| {
        NexusToolError::BadInput(format!(
            "Nessuna connessione DB per il progetto {}",
            project_id
        ))
    })?;

    if engine != "postgres" {
        return Err(NexusToolError::BadInput(format!(
            "Engine '{}' non supportato",
            engine
        )));
    }

    let dsn = String::from_utf8(secret_bytes)
        .map_err(|_| NexusToolError::BadInput("connection_secret non UTF-8".into()))?;

    db_helper::normalize_dsn_pub(dsn.trim())
        .map_err(|e| NexusToolError::BadInput(format!("DSN: {}", e)))
}
