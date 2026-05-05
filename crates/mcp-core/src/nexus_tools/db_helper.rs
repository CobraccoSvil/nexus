//! Helper condiviso per i tool `database::*`: ottiene una connessione
//! PostgreSQL da `DATABASE_URL` con limite stretto (max 2 conn, 5s timeout).
//!
//! Uso tipico:
//! ```ignore
//! let pool = match db_helper::get_pool().await {
//!     Ok(p) => p,
//!     Err(msg) => return Ok(serde_json::json!({"ok": false, "error": msg})),
//! };
//! ```
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Parole chiave DDL che richiedono il blocco quando il target è un progetto utente.
const DDL_KEYWORDS: &[&str] = &[
    "CREATE TABLE", "CREATE INDEX", "CREATE VIEW", "CREATE SEQUENCE",
    "CREATE TYPE", "CREATE FUNCTION", "CREATE TRIGGER", "CREATE SCHEMA",
    "ALTER TABLE", "ALTER COLUMN", "ALTER INDEX",
    "DROP TABLE", "DROP INDEX", "DROP VIEW", "DROP COLUMN",
    "DROP SCHEMA", "DROP SEQUENCE", "DROP TYPE", "DROP FUNCTION",
    "DROP TRIGGER",
    "TRUNCATE", "RENAME TABLE", "RENAME COLUMN",
];

/// Controlla se un testo SQL contiene istruzioni DDL che modificano lo schema.
#[allow(dead_code)]
pub fn contains_ddl_statement(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    DDL_KEYWORDS.iter().any(|kw| upper.contains(kw))
}

/// Restituisce payload JSON strutturato DDL_BLOCKED serializzato come stringa.
/// Usato da agent_loop.rs per intercettare DDL diretto sui progetti.
#[allow(dead_code)]
pub fn ddl_blocked_response(project_id: uuid::Uuid) -> String {
    serde_json::json!({
        "error": "DDL_BLOCKED",
        "message": "Modifica schema bloccata. Nexus richiede l'uso del migration runner per modifiche schema sui progetti utente.",
        "suggested_tool": "project_db_create_migration",
        "override_endpoint": format!("/api/projects/{}/db/override-request", project_id),
        "hint": "Usa project_db_create_migration({\"name\": \"...\", \"sql\": \"...\"}) per creare una migration tracciabile."
    }).to_string()
}

pub async fn get_pool() -> Result<PgPool, String> {
    let db_url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL not set".to_string())?;
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&db_url)
        .await
        .map_err(|e| format!("connect failed: {}", e))
}
