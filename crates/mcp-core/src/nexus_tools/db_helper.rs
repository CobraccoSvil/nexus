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

// ── DB isolation per-progetto (PR hardening) ─────────────────────────────

/// Crea un ruolo PostgreSQL dedicato e un database isolato per il progetto,
/// con REVOKE su database di infrastruttura (nexus, postgres).
///
/// Chiamata da `project_db_set_connection` quando `hosting_mode='internal'` e
/// il DSN punta allo stesso cluster Postgres di Nexus.
///
/// Ritorna il DSN con le credenziali del ruolo dedicato.
/// Se il ruolo esiste gia', ritorna il DSN esistente senza ricreare.
pub async fn ensure_project_db_isolation(
    nexus_pool: &PgPool,
    project_id: uuid::Uuid,
    original_dsn: &str,
) -> Result<String, String> {
    // Controlla se il DSN punta allo stesso cluster di Nexus
    let nexus_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nexus:nexus@localhost:5433/nexus".to_string());
    let nexus_host = extract_host_port(&nexus_url);
    let target_host = extract_host_port(original_dsn);

    // Se il DSN punta a un cluster diverso, non serve isolation — il progetto
    // ha il suo Postgres separato (es. container Docker dedicato).
    if nexus_host != target_host {
        tracing::debug!(
            project_id = %project_id,
            nexus = %nexus_host,
            target = %target_host,
            "DB isolation non necessaria: cluster diverso"
        );
        return Ok(original_dsn.to_string());
    }

    let short_id = &project_id.to_string()[..8]; // primi 8 char del UUID
    let role_name = format!("proj_{}", short_id);
    let db_name = format!("proj_{}", short_id);
    let password = generate_db_password();

    // Controlla se il ruolo esiste gia'
    let role_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname = $1)",
    )
    .bind(&role_name)
    .fetch_one(nexus_pool)
    .await
    .unwrap_or(false);

    if role_exists {
        // Ruolo gia' creato: aggiorna solo la password e ritorna il DSN
        let alter = format!("ALTER ROLE {} PASSWORD '{}'", role_name, password);
        sqlx::query(&alter)
            .execute(nexus_pool)
            .await
            .map_err(|e| format!("ALTER ROLE {} password: {}", role_name, e))?;

        tracing::info!(
            project_id = %project_id,
            role = %role_name,
            "DB isolation: ruolo gia' esistente, password aggiornata"
        );
    } else {
        // Crea ruolo + database
        let create_role = format!(
            "CREATE ROLE {} LOGIN PASSWORD '{}' NOSUPERUSER NOCREATEDB NOCREATEROLE",
            role_name, password
        );
        sqlx::query(&create_role)
            .execute(nexus_pool)
            .await
            .map_err(|e| format!("CREATE ROLE {}: {}", role_name, e))?;

        tracing::info!(
            project_id = %project_id,
            role = %role_name,
            "DB isolation: ruolo creato"
        );
    }

    // Controlla se il database esiste
    let db_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
    )
    .bind(&db_name)
    .fetch_one(nexus_pool)
    .await
    .unwrap_or(false);

    if !db_exists {
        let create_db = format!(
            "CREATE DATABASE {} OWNER {} ENCODING 'UTF8'",
            db_name, role_name
        );
        sqlx::query(&create_db)
            .execute(nexus_pool)
            .await
            .map_err(|e| format!("CREATE DATABASE {}: {}", db_name, e))?;

        tracing::info!(
            project_id = %project_id,
            db = %db_name,
            "DB isolation: database creato"
        );
    }

    // REVOKE: impedisci al ruolo progetto di accedere ai DB infrastruttura
    for protected_db in &["nexus", "postgres"] {
        let revoke = format!(
            "REVOKE ALL PRIVILEGES ON DATABASE {} FROM {}",
            protected_db, role_name
        );
        // Best-effort: il DB potrebbe non esistere su cluster alternativi
        let _ = sqlx::query(&revoke).execute(nexus_pool).await;
    }

    // Costruisci DSN con credenziali isolate
    let (host, port) = parse_host_port_from_dsn(original_dsn);
    let isolated_dsn = format!(
        "postgres://{}:{}@{}:{}/{}",
        role_name, password, host, port, db_name
    );

    // Audit
    crate::security::record_audit(
        crate::security::AuditEntry::allowed(project_id, "db_isolation_setup", "db")
            .with_resource(db_name.clone())
            .with_details(serde_json::json!({
                "role": role_name,
                "database": db_name,
                "cluster": format!("{}:{}", host, port),
            })),
    );

    Ok(isolated_dsn)
}

/// Estrae host:port da un DSN postgres://user:pass@host:port/db
fn extract_host_port(dsn: &str) -> String {
    let (host, port) = parse_host_port_from_dsn(dsn);
    format!("{}:{}", host, port)
}

/// Parse host e port da DSN PostgreSQL. Default: localhost:5432
fn parse_host_port_from_dsn(dsn: &str) -> (String, u16) {
    // Formato: postgres://user:pass@host:port/db?params
    let after_scheme = dsn
        .strip_prefix("postgres://")
        .or_else(|| dsn.strip_prefix("postgresql://"))
        .unwrap_or(dsn);

    // Rimuovi user:pass@
    let after_auth = after_scheme
        .split_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(after_scheme);

    // Estrai host:port (prima di / o ?)
    let without_path = after_auth
        .split_once('/')
        .map(|(hp, _)| hp)
        .unwrap_or(after_auth);
    let host_port = without_path
        .split_once('?')
        .map(|(hp, _)| hp)
        .unwrap_or(without_path);

    if let Some((host, port_str)) = host_port.rsplit_once(':') {
        let port = port_str.parse::<u16>().unwrap_or(5432);
        (host.to_string(), port)
    } else {
        (host_port.to_string(), 5432)
    }
}

/// Genera una password casuale sicura per il ruolo PostgreSQL del progetto.
fn generate_db_password() -> String {
    use std::fmt::Write;
    use std::io::Read;
    let mut rng_bytes = [0u8; 24];
    // Usa /dev/urandom per generare bytes casuali (read_exact, non read intero file!)
    let read_ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut rng_bytes))
        .is_ok();
    if !read_ok {
        // Fallback: timestamp-based (meno sicuro ma sufficiente per dev locale)
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, b) in ts.to_le_bytes().iter().cycle().take(24).enumerate() {
            rng_bytes[i] = *b;
        }
    }
    let mut hex = String::with_capacity(48);
    for b in &rng_bytes {
        let _ = write!(hex, "{:02x}", b);
    }
    hex
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

    #[test]
    fn test_parse_host_port_standard() {
        let (h, p) = parse_host_port_from_dsn("postgres://user:pass@db.example.com:5433/mydb");
        assert_eq!(h, "db.example.com");
        assert_eq!(p, 5433);
    }

    #[test]
    fn test_parse_host_port_default() {
        let (h, p) = parse_host_port_from_dsn("postgres://user:pass@localhost/mydb");
        assert_eq!(h, "localhost");
        assert_eq!(p, 5432);
    }

    #[test]
    fn test_parse_host_port_with_params() {
        let (h, p) = parse_host_port_from_dsn("postgres://u:p@myhost:5434/db?sslmode=disable");
        assert_eq!(h, "myhost");
        assert_eq!(p, 5434);
    }

    #[test]
    fn test_extract_host_port_matches() {
        let a = extract_host_port("postgres://nexus:nexus@localhost:5433/nexus");
        let b = extract_host_port("postgres://other:other@localhost:5433/other_db");
        assert_eq!(a, b); // stesso cluster: isolation necessaria
    }

    #[test]
    fn test_extract_host_port_differs() {
        let a = extract_host_port("postgres://nexus:nexus@localhost:5433/nexus");
        let b = extract_host_port("postgres://proj:proj@remotehost:5434/proj_db");
        assert_ne!(a, b); // cluster diversi: no isolation
    }

    #[test]
    fn test_generate_db_password_length() {
        let pwd = generate_db_password();
        assert_eq!(pwd.len(), 48); // 24 bytes * 2 hex chars
        assert!(pwd.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
