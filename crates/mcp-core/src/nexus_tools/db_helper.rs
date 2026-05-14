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
pub fn contains_ddl_statement(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    DDL_KEYWORDS.iter().any(|kw| upper.contains(kw))
}

/// Restituisce payload JSON strutturato DDL_BLOCKED serializzato come stringa.
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

// ── Pool per DB del progetto ─────────────────────────────────────────

/// Apre un pool temporaneo verso un DSN arbitrario (PostgreSQL).
/// Supporta sia formato `postgres://` che ADO.NET (`Server=...;...`).
pub async fn get_pool_for_dsn(dsn: &str) -> Result<PgPool, String> {
    let normalized = normalize_dsn(dsn)?;
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&normalized)
        .await
        .map_err(|e| format!("connect failed: {}", e))
}

/// Cerca la connection_string nella tabella `project_database_config`
/// per il progetto dato, poi apre un pool temporaneo verso quel DB.
pub async fn get_pool_for_project(
    nexus_pool: &PgPool,
    project_id: uuid::Uuid,
) -> Result<PgPool, String> {
    use sqlx::Row;

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
    .map_err(|e| format!("lookup project_database_config failed: {}", e))?;

    let (secret_bytes, engine) = row.ok_or_else(|| {
        format!(
            "Nessuna connessione DB configurata per il progetto {}. Usa project_db_set_connection per configurarla.",
            project_id
        )
    })?;

    if engine != "postgres" {
        return Err(format!(
            "Engine '{}' non supportato. Solo PostgreSQL e' supportato.",
            engine
        ));
    }

    let dsn = String::from_utf8(secret_bytes)
        .map_err(|_| "connection_secret non e' UTF-8 valido".to_string())?;

    get_pool_for_dsn(dsn.trim()).await
}

/// Wrapper pubblico di `normalize_dsn` per uso da altri moduli.
pub fn normalize_dsn_pub(dsn: &str) -> Result<String, String> {
    normalize_dsn(dsn)
}

/// Normalizza un DSN: se e' gia' `postgres://` lo passa invariato,
/// se e' in formato ADO.NET lo converte.
fn normalize_dsn(dsn: &str) -> Result<String, String> {
    let trimmed = dsn.trim();

    // Gia' formato URI standard
    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        return Ok(trimmed.to_string());
    }

    // Prova formato ADO.NET: Server=host;Port=5432;Database=db;User Id=u;Password=p;
    if trimmed.contains('=') && trimmed.contains(';') {
        return parse_ado_net_dsn(trimmed);
    }

    Err(format!(
        "Formato DSN non riconosciuto. Atteso postgres://... o Server=...;Port=...;Database=...;User Id=...;Password=...;"
    ))
}

fn parse_ado_net_dsn(dsn: &str) -> Result<String, String> {
    let mut server = "";
    let mut port = "5432";
    let mut database = "";
    let mut user = "";
    let mut password = "";

    for part in dsn.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((key, value)) = part.split_once('=') {
            let key_lower = key.trim().to_lowercase();
            let val = value.trim();
            match key_lower.as_str() {
                "server" | "host" => server = val,
                "port" => port = val,
                "database" | "initial catalog" => database = val,
                "user id" | "uid" | "user" | "username" => user = val,
                "password" | "pwd" => password = val,
                _ => {} // ignora parametri sconosciuti
            }
        }
    }

    if server.is_empty() || database.is_empty() || user.is_empty() {
        return Err(
            "DSN ADO.NET incompleto: servono almeno Server, Database e User Id".to_string(),
        );
    }

    // URL-encode user e password per caratteri speciali
    let encoded_user = urlencoding::encode(user);
    let encoded_pass = urlencoding::encode(password);

    Ok(format!(
        "postgres://{}:{}@{}:{}/{}",
        encoded_user, encoded_pass, server, port, database
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_postgres_url_passthrough() {
        let dsn = "postgres://user:pass@localhost:5432/mydb";
        assert_eq!(normalize_dsn(dsn).unwrap(), dsn);
    }

    #[test]
    fn test_normalize_postgresql_url_passthrough() {
        let dsn = "postgresql://user:pass@localhost/mydb";
        assert_eq!(normalize_dsn(dsn).unwrap(), dsn);
    }

    #[test]
    fn test_normalize_ado_net_basic() {
        let dsn = "Server=db.example.com;Port=5433;Database=mydb;User Id=admin;Password=secret;";
        let result = normalize_dsn(dsn).unwrap();
        assert_eq!(result, "postgres://admin:secret@db.example.com:5433/mydb");
    }

    #[test]
    fn test_normalize_ado_net_default_port() {
        let dsn = "Server=localhost;Database=test;User Id=u;Password=p;";
        let result = normalize_dsn(dsn).unwrap();
        assert_eq!(result, "postgres://u:p@localhost:5432/test");
    }

    #[test]
    fn test_normalize_ado_net_special_chars_password() {
        let dsn = "Server=host;Database=db;User Id=user;Password=p@ss w0rd!;";
        let result = normalize_dsn(dsn).unwrap();
        assert!(result.contains("p%40ss%20w0rd%21"));
    }

    #[test]
    fn test_normalize_ado_net_missing_server() {
        let dsn = "Database=db;User Id=u;Password=p;";
        assert!(normalize_dsn(dsn).is_err());
    }

    #[test]
    fn test_normalize_unknown_format() {
        let dsn = "just-a-random-string";
        assert!(normalize_dsn(dsn).is_err());
    }

    #[test]
    fn test_contains_ddl_detects_create_table() {
        assert!(contains_ddl_statement("CREATE TABLE users (id int)"));
        assert!(contains_ddl_statement("select 1; DROP TABLE users"));
    }

    #[test]
    fn test_contains_ddl_allows_select() {
        assert!(!contains_ddl_statement("SELECT * FROM users"));
        assert!(!contains_ddl_statement("WITH cte AS (SELECT 1) SELECT * FROM cte"));
    }
}
