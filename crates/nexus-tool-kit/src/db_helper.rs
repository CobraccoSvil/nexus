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
use serde_json::{json, Value};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use super::{NexusToolContext, NexusToolError};

/// Estrae l'argomento `schema` (default `public`) dagli args JSON dei tool
/// db_*. Punto unico (regola L) per db_table_list / db_view_list / ecc.
pub fn schema_arg(args: &Value) -> String {
    args.get("schema")
        .and_then(Value::as_str)
        .unwrap_or("public")
        .to_string()
}

/// Estrae l'argomento `limit` con default e clamp [1, max]. Punto unico per
/// db_bloat_check / db_dead_tuples e tool simili.
pub fn limit_arg(args: &Value, default: i64, max: i64) -> i64 {
    args.get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(default)
        .clamp(1, max)
}

/// Valore da bindare come unico parametro `$1` nelle query di catalogo.
pub enum CatalogBind {
    Text(String),
    Int(i64),
}

/// Tipo SQL della colonna proiettata da `list_catalog_rows`.
pub enum CatalogColKind {
    Text,
    TextOpt,
    Int,
    Float,
}

/// Proiezione `colonna SQL -> chiave JSON` per `list_catalog_rows`.
pub struct CatalogCol {
    pub col: &'static str,
    pub key: &'static str,
    pub kind: CatalogColKind,
}

impl CatalogCol {
    pub const fn text(col: &'static str, key: &'static str) -> Self {
        Self {
            col,
            key,
            kind: CatalogColKind::Text,
        }
    }
    pub const fn text_opt(col: &'static str, key: &'static str) -> Self {
        Self {
            col,
            key,
            kind: CatalogColKind::TextOpt,
        }
    }
    pub const fn int(col: &'static str, key: &'static str) -> Self {
        Self {
            col,
            key,
            kind: CatalogColKind::Int,
        }
    }
    pub const fn float(col: &'static str, key: &'static str) -> Self {
        Self {
            col,
            key,
            kind: CatalogColKind::Float,
        }
    }
}

/// Esegue una query di catalogo Postgres con un singolo bind. Gli errori
/// pool/query diventano il JSON `{"ok": false, "error": ...}` canonico dei
/// tool db_* (ramo `Err`).
async fn fetch_catalog(sql: &str, bind: CatalogBind) -> Result<Vec<PgRow>, Value> {
    let pool = match get_pool().await {
        Ok(p) => p,
        Err(e) => return Err(json!({"ok": false, "error": e})),
    };
    let query = sqlx::query(sql);
    let query = match bind {
        CatalogBind::Text(s) => query.bind(s),
        CatalogBind::Int(i) => query.bind(i),
    };
    match query.fetch_all(&pool).await {
        Ok(r) => Ok(r),
        Err(e) => Err(json!({"ok": false, "error": format!("query: {}", e)})),
    }
}

/// Lista di valori testuali da una singola colonna di catalogo (le righe non
/// decodificabili vengono scartate). Punto unico (regola L) per
/// db_table_list / db_view_list / db_seq_list.
pub async fn list_catalog_strings(
    sql: &str,
    bind: CatalogBind,
    col: &str,
) -> Result<Vec<String>, Value> {
    let rows = fetch_catalog(sql, bind).await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>(col).ok())
        .collect())
}

/// Lista di oggetti JSON proiettati secondo `cols`. Punto unico (regola L)
/// per db_index_list / db_constraint_list / db_foreign_keys / db_bloat_check
/// / db_dead_tuples.
pub async fn list_catalog_rows(
    sql: &str,
    bind: CatalogBind,
    cols: &[CatalogCol],
) -> Result<Vec<Value>, Value> {
    let rows = fetch_catalog(sql, bind).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for c in cols {
                let v = match c.kind {
                    CatalogColKind::Text => {
                        json!(r.try_get::<String, _>(c.col).unwrap_or_default())
                    }
                    CatalogColKind::TextOpt => {
                        json!(r.try_get::<Option<String>, _>(c.col).unwrap_or_default())
                    }
                    CatalogColKind::Int => json!(r.try_get::<i64, _>(c.col).unwrap_or(0)),
                    CatalogColKind::Float => json!(r.try_get::<f64, _>(c.col).unwrap_or(0.0)),
                };
                obj.insert(c.key.to_string(), v);
            }
            Value::Object(obj)
        })
        .collect())
}

/// Esegue una query di catalogo che proietta una lista di tabelle e ne incapsula
/// l'esito nel JSON standard `{ok, count, tables}`. Punto unico (regola L) per i
/// tool `db_bloat_check` / `db_dead_tuples`, che condividevano lo stesso wrapping
/// `match list_catalog_rows { ... } -> Ok(json!(...))`.
pub async fn list_tables_response(
    sql: &str,
    bind: CatalogBind,
    cols: &[CatalogCol],
) -> Result<Value, NexusToolError> {
    let items = match list_catalog_rows(sql, bind, cols).await {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    Ok(json!({"ok": true, "count": items.len(), "tables": items}))
}

/// Introspezione tabelle+colonne via `information_schema` su un pool gia'
/// aperto. Con `with_defaults_and_estimates` aggiunge `column_default` per
/// colonna e `estimated_row_count` per tabella (variante project_db_schema).
/// Punto unico (regola L) per db_schema_inspect / project_db_schema.
pub async fn inspect_schema_tables(
    pool: &PgPool,
    schema: &str,
    table_filter: Option<&str>,
    with_defaults_and_estimates: bool,
) -> Result<Vec<Value>, NexusToolError> {
    let mut tables_q = sqlx::query(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = $1 AND table_type='BASE TABLE'
         ORDER BY table_name",
    )
    .bind(schema)
    .fetch_all(pool)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("query tables failed: {}", e)))?;

    if let Some(f) = table_filter {
        tables_q.retain(|r| {
            r.try_get::<String, _>("table_name")
                .map(|n| n == f)
                .unwrap_or(false)
        });
    }

    let cols_sql = if with_defaults_and_estimates {
        "SELECT column_name, data_type, is_nullable, column_default
         FROM information_schema.columns
         WHERE table_schema = $1 AND table_name = $2
         ORDER BY ordinal_position"
    } else {
        "SELECT column_name, data_type, is_nullable
         FROM information_schema.columns
         WHERE table_schema = $1 AND table_name = $2
         ORDER BY ordinal_position"
    };

    let mut tables_out: Vec<Value> = Vec::with_capacity(tables_q.len());
    for row in tables_q {
        let table_name: String = row
            .try_get("table_name")
            .map_err(|e| NexusToolError::BadInput(format!("row decode: {}", e)))?;
        let cols = sqlx::query(cols_sql)
            .bind(schema)
            .bind(&table_name)
            .fetch_all(pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("query cols failed: {}", e)))?;
        let cols_json: Vec<Value> = cols
            .iter()
            .map(|c| {
                let name: String = c.try_get("column_name").unwrap_or_default();
                let dtype: String = c.try_get("data_type").unwrap_or_default();
                let nullable: String = c.try_get("is_nullable").unwrap_or_default();
                if with_defaults_and_estimates {
                    let default: Option<String> = c.try_get("column_default").ok();
                    json!({
                        "name": name,
                        "type": dtype,
                        "nullable": nullable == "YES",
                        "default": default,
                    })
                } else {
                    json!({
                        "name": name,
                        "type": dtype,
                        "nullable": nullable == "YES",
                    })
                }
            })
            .collect();

        if with_defaults_and_estimates {
            // Conta righe (best-effort, ignora errore)
            let row_count: Option<i64> = sqlx::query_scalar(&format!(
                "SELECT reltuples::bigint AS estimate
                 FROM pg_class
                 WHERE oid = '\"{}\".\"{}\"'::regclass",
                schema.replace('"', "\"\""),
                table_name.replace('"', "\"\"")
            ))
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            tables_out.push(json!({
                "name": table_name,
                "columns": cols_json,
                "estimated_row_count": row_count,
            }));
        } else {
            tables_out.push(json!({
                "name": table_name,
                "columns": cols_json,
            }));
        }
    }
    Ok(tables_out)
}

/// Valida che un identifier SQL sia ASCII alfanumerico + underscore (no punto,
/// no spazi, no virgolette). Punto unico (regola L, S76) per i tool DB stretti
/// che NON accettano notazione schema.table (es. db_table_count, db_table_size).
pub fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Estrae `(schema, table)` da un args JSON dei tool DB, applicando default
/// `schema = "public"` e validando entrambi con `ident_ok`. Punto unico
/// (regola L, S76) per db_table_count + db_table_size + altri tool simili.
pub fn extract_schema_table(args: &serde_json::Value) -> Result<(String, String), NexusToolError> {
    let table = args
        .get("table")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| NexusToolError::BadInput("table required".into()))?
        .to_string();
    let schema = args
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("public")
        .to_string();
    if !ident_ok(&table) || !ident_ok(&schema) {
        return Err(NexusToolError::BadInput("invalid identifier".into()));
    }
    Ok((schema, table))
}

/// Valida che un nome tabella SQL contenga solo caratteri sicuri (alfanumerici,
/// underscore, punto per schema.table). Punto unico (regola L, S75): prima
/// duplicato in project_db_analyze + project_db_vacuum.
pub fn validate_table_name(t: &str) -> Result<(), NexusToolError> {
    if !t
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return Err(NexusToolError::BadInput(
            "Nome tabella contiene caratteri non validi".into(),
        ));
    }
    Ok(())
}

/// Apre un pool sul DB di progetto risolvendo via `db_helper::get_pool` +
/// `get_pool_for_project`, e chiude il pool nexus intermedio. Punto unico
/// (regola L, S75): prima duplicato in project_db_analyze + project_db_vacuum
/// + altri tool DB del progetto.
pub async fn open_project_pool(ctx: &NexusToolContext) -> Result<PgPool, NexusToolError> {
    let nexus_pool = get_pool()
        .await
        .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;
    let project_pool = get_pool_for_project(&nexus_pool, ctx.project_id)
        .await
        .map_err(NexusToolError::BadInput)?;
    nexus_pool.close().await;
    Ok(project_pool)
}

/// Parole chiave DDL che richiedono il blocco quando il target è un progetto utente.
const DDL_KEYWORDS: &[&str] = &[
    "CREATE TABLE",
    "CREATE INDEX",
    "CREATE VIEW",
    "CREATE SEQUENCE",
    "CREATE TYPE",
    "CREATE FUNCTION",
    "CREATE TRIGGER",
    "CREATE SCHEMA",
    "ALTER TABLE",
    "ALTER COLUMN",
    "ALTER INDEX",
    "DROP TABLE",
    "DROP INDEX",
    "DROP VIEW",
    "DROP COLUMN",
    "DROP SCHEMA",
    "DROP SEQUENCE",
    "DROP TYPE",
    "DROP FUNCTION",
    "DROP TRIGGER",
    "TRUNCATE",
    "RENAME TABLE",
    "RENAME COLUMN",
];

/// Controlla se un testo SQL contiene istruzioni DDL che modificano lo schema.
pub fn contains_ddl_statement(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    DDL_KEYWORDS.iter().any(|kw| upper.contains(kw))
}

pub async fn get_pool() -> Result<PgPool, String> {
    let db_url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL not set".to_string())?;
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&db_url)
        .await
        .map_err(|e| format!("connect failed: {}", e))
}

// ── Pool per DB del progetto ─────────────────────────────────────────

/// Cerca la connection_string nella tabella `project_database_config`
/// per il progetto dato, poi apre un pool temporaneo verso quel DB.
pub async fn get_pool_for_project(
    nexus_pool: &PgPool,
    project_id: uuid::Uuid,
) -> Result<PgPool, String> {
    

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

    // Applica max_db_pool_size dalla quota del progetto (cache 60s, no round-trip
    // aggiuntivo in hot path). Se la quota non esiste usa emergency_default (10).
    let quota = crate::quotas::load_quota(nexus_pool, project_id).await;
    let max_conn = (quota.max_db_pool_size.max(1) as u32).min(50); // cap ragionevole

    let normalized = normalize_dsn(dsn.trim())?;
    PgPoolOptions::new()
        .max_connections(max_conn)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&normalized)
        .await
        .map_err(|e| format!("connect failed: {}", e))
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

    Err("Formato DSN non riconosciuto. Atteso postgres://... o Server=...;Port=...;Database=...;User Id=...;Password=...;".to_string())
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
    let role_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname = $1)")
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
    let db_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
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
    crate::audit::record_audit(
        crate::audit::AuditEntry::allowed(project_id, "db_isolation_setup", "db")
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
        assert!(!contains_ddl_statement(
            "WITH cte AS (SELECT 1) SELECT * FROM cte"
        ));
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
