//! Esecuzione SQL ad-hoc sul DB applicativo del progetto.
//!
//! Modulo condiviso tra:
//!   - tool MCP `nexus_db_query`/`nexus_db_tables`/`nexus_db_describe`
//!     (`crates/mcp-core/src/agent_tools/project_db_query.rs`), invocato
//!     dall'agente via chat.
//!   - endpoint REST `POST /api/projects/:id/db/query` esposto da
//!     `project_db_routes.rs`, invocato dal pannello SQL del frontend.
//!
//! La logica deve stare in UN SOLO posto (regola H: niente duplicazione).
//!
//! ## Sicurezza
//!
//! - La connessione e' SEMPRE risolta da `project_database_config` (filtro
//!   `is_primary = true`). Mai da input utente/modello.
//! - Guard-rail anti-contaminazione: se la URL punta al DB infrastruttura
//!   Nexus (db `nexus` su porta 5433, oppure hostname `postgres-nexus`/
//!   `ideai-postgres-nexus`), l'esecuzione viene RIFIUTATA. Un tool
//!   applicativo non deve mai toccare il DB di sistema (regola E:
//!   isolamento progetti).
//! - Limiti: timeout query 30s, max 1000 righe ritornate, 20_000 caratteri
//!   per cella stringa.

use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Column, PgPool, Row, TypeInfo};
use std::time::Duration;
use uuid::Uuid;

pub const QUERY_TIMEOUT_SECS: u64 = 30;
pub const MAX_ROWS: usize = 1000;
pub const MAX_CELL_CHARS: usize = 20_000;

/// Risolve la connection string del DB primario del progetto, applicando il
/// guard-rail anti-contaminazione verso il DB Nexus.
///
/// Legge `connection_secret` (bytea con la URL raw) dal pool Nexus passato.
pub async fn resolve_project_conn(db: &PgPool, project_id: Uuid) -> Result<String, String> {
    let row = sqlx::query(
        "SELECT connection_secret FROM project_database_config \
         WHERE project_id = $1 AND is_primary = true \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("query project_database_config fallita: {e}"))?
    .ok_or_else(|| {
        "Nessun database configurato per questo progetto. Usa il pannello \
         Database del progetto per aggiungere una connessione, oppure esegui \
         un comando che avvii il DB applicativo (auto-provisioning)."
            .to_string()
    })?;

    let secret: Option<Vec<u8>> = row.try_get("connection_secret").unwrap_or(None);
    let conn = secret
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "connection_secret vuoto o non decodificabile".to_string())?;

    // Guard-rail: blocca connessioni verso il DB infrastruttura Nexus.
    let lower = conn.to_lowercase();
    let touches_nexus_db = (lower.contains("/nexus") || lower.contains("database=nexus"))
        && (lower.contains(":5433")
            || lower.contains("postgres-nexus")
            || lower.contains("ideai-postgres-nexus"));
    if touches_nexus_db {
        return Err(
            "SICUREZZA: la connessione del progetto punta al DB infrastruttura \
             Nexus. Operazione rifiutata per isolamento. Configura un DB \
             applicativo dedicato per il progetto."
                .to_string(),
        );
    }
    Ok(conn)
}

/// Apre un pool sqlx (max 2 conn) verso il DB del progetto.
pub async fn open_pool(conn: &str) -> Result<PgPool, String> {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(conn)
        .await
        .map_err(|e| format!("connessione DB progetto fallita: {e}"))
}

/// Converte una cella di PgRow in serde_json::Value in base al tipo Postgres.
/// I tipi non gestiti esplicitamente ricadono su String; se anche quella
/// fallisce, ritorna un marcatore di tipo.
pub fn cell_to_json(row: &sqlx::postgres::PgRow, idx: usize, type_name: &str) -> Value {
    macro_rules! try_as {
        ($t:ty) => {
            row.try_get::<Option<$t>, _>(idx).ok().map(|o| o.map(Value::from))
        };
    }
    let val: Option<Option<Value>> = match type_name {
        "BOOL" => row.try_get::<Option<bool>, _>(idx).ok().map(|o| o.map(Value::from)),
        "INT2" => row
            .try_get::<Option<i16>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as i64))),
        "INT4" => row
            .try_get::<Option<i32>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as i64))),
        "INT8" => row.try_get::<Option<i64>, _>(idx).ok().map(|o| o.map(Value::from)),
        "FLOAT4" => row
            .try_get::<Option<f32>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as f64))),
        "FLOAT8" => try_as!(f64),
        "NUMERIC" => try_as!(f64),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "CHAR" | "CITEXT" => try_as!(String),
        "UUID" => row
            .try_get::<Option<uuid::Uuid>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "JSON" | "JSONB" => row.try_get::<Option<Value>, _>(idx).ok(),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_rfc3339()))),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "BYTEA" => row
            .try_get::<Option<Vec<u8>>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(format!("\\x{} ({} byte)", hex_prefix(&v), v.len())))),
        _ => None,
    };
    match val {
        Some(Some(v)) => clamp_cell(v),
        Some(None) => Value::Null,
        None => match row.try_get::<Option<String>, _>(idx) {
            Ok(Some(s)) => clamp_cell(Value::from(s)),
            Ok(None) => Value::Null,
            Err(_) => Value::from(format!("<tipo non serializzabile: {type_name}>")),
        },
    }
}

/// Tronca celle stringa enormi per non saturare il context window.
pub fn clamp_cell(v: Value) -> Value {
    if let Value::String(s) = &v {
        if s.chars().count() > MAX_CELL_CHARS {
            let truncated: String = s.chars().take(MAX_CELL_CHARS).collect();
            return Value::from(format!("{truncated}...[TRONCATO]"));
        }
    }
    v
}

fn hex_prefix(bytes: &[u8]) -> String {
    bytes.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Determina se la query e' di sola lettura (SELECT / WITH ... SELECT / SHOW /
/// EXPLAIN). Usato per scegliere fetch_all vs execute.
pub fn is_read_only(sql: &str) -> bool {
    let t = sql.trim_start().to_lowercase();
    t.starts_with("select")
        || t.starts_with("show")
        || t.starts_with("explain")
        || t.starts_with("with")
}

/// Determina il "kind" semantico della statement per il dispatcher event
/// `ProjectEvent::DbQueryRun.statement_kind` e per il tagging KB.
///
/// Valori: "select", "insert", "update", "delete", "ddl", "tx", "other".
pub fn classify_statement(sql: &str) -> &'static str {
    let t = sql.trim_start().to_lowercase();
    if t.starts_with("select") || t.starts_with("with") || t.starts_with("show") || t.starts_with("explain") {
        "select"
    } else if t.starts_with("insert") {
        "insert"
    } else if t.starts_with("update") {
        "update"
    } else if t.starts_with("delete") {
        "delete"
    } else if t.starts_with("begin")
        || t.starts_with("commit")
        || t.starts_with("rollback")
        || t.starts_with("savepoint")
    {
        "tx"
    } else if t.starts_with("create")
        || t.starts_with("alter")
        || t.starts_with("drop")
        || t.starts_with("truncate")
        || t.starts_with("rename")
    {
        "ddl"
    } else {
        "other"
    }
}

/// Esito strutturato di un'esecuzione SQL.
#[derive(Debug)]
pub struct QueryExecOutcome {
    pub mode: &'static str,            // "read" | "write"
    pub statement_kind: String,        // select|insert|update|delete|ddl|tx|other
    pub columns: Vec<Value>,           // [{name,type}, ...]
    pub rows: Vec<Value>,              // [{col:val, ...}, ...]
    pub row_count: usize,              // numero righe ritornate (read) o 0 (write)
    pub rows_affected: Option<u64>,    // solo write
    pub truncated: bool,               // true se ho clippato per max_rows
    pub duration_ms: u64,
}

/// Errore di esecuzione query.
#[derive(Debug)]
pub enum QueryExecError {
    /// Connessione progetto non risolvibile o guard-rail anti-Nexus scattato.
    ConnectionError(String),
    /// Timeout server-side (oltre QUERY_TIMEOUT_SECS).
    Timeout,
    /// Errore SQL dal driver (sintassi, vincoli, permessi, ecc.).
    Sql(String),
}

impl QueryExecError {
    pub fn message(&self) -> String {
        match self {
            QueryExecError::ConnectionError(m) => m.clone(),
            QueryExecError::Timeout => format!("timeout query dopo {QUERY_TIMEOUT_SECS}s"),
            QueryExecError::Sql(m) => format!("errore SQL: {m}"),
        }
    }
}

/// Esegue una query SQL completa. Ritorna un esito strutturato.
///
/// `params` sono bindati come TEXT (NULL -> bind null). Il chiamante puo' usare
/// cast espliciti nel SQL quando serve un tipo non-testo: es.
/// `INSERT INTO t(qty) VALUES ($1::int)`.
///
/// `max_rows` viene clippato a [`MAX_ROWS`].
pub async fn execute_query(
    db: &PgPool,
    project_id: Uuid,
    sql: &str,
    params: &[Option<String>],
    max_rows: Option<usize>,
) -> Result<QueryExecOutcome, QueryExecError> {
    let max_rows = max_rows.map(|n| n.min(MAX_ROWS)).unwrap_or(MAX_ROWS);

    let conn = resolve_project_conn(db, project_id)
        .await
        .map_err(QueryExecError::ConnectionError)?;
    let pool = open_pool(&conn).await.map_err(QueryExecError::ConnectionError)?;

    let read_only = is_read_only(sql);
    let statement_kind = classify_statement(sql).to_string();

    let mut q = sqlx::query(sql);
    for p in params {
        q = q.bind(p.clone());
    }

    let started = std::time::Instant::now();

    let outcome = if read_only {
        let fetch =
            tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.fetch_all(&pool)).await;
        let rows = match fetch {
            Err(_) => {
                pool.close().await;
                return Err(QueryExecError::Timeout);
            }
            Ok(Err(e)) => {
                pool.close().await;
                return Err(QueryExecError::Sql(e.to_string()));
            }
            Ok(Ok(r)) => r,
        };
        let duration_ms = started.elapsed().as_millis() as u64;

        let truncated = rows.len() > max_rows;
        let slice = &rows[..rows.len().min(max_rows)];

        let columns: Vec<Value> = slice
            .first()
            .map(|r| {
                r.columns()
                    .iter()
                    .map(|c| json!({"name": c.name(), "type": c.type_info().name()}))
                    .collect()
            })
            .unwrap_or_default();

        let out_rows: Vec<Value> = slice
            .iter()
            .map(|row| {
                let mut obj = serde_json::Map::new();
                for (i, col) in row.columns().iter().enumerate() {
                    let type_name = col.type_info().name();
                    obj.insert(col.name().to_string(), cell_to_json(row, i, type_name));
                }
                Value::Object(obj)
            })
            .collect();

        QueryExecOutcome {
            mode: "read",
            statement_kind,
            row_count: out_rows.len(),
            columns,
            rows: out_rows,
            rows_affected: None,
            truncated,
            duration_ms,
        }
    } else {
        let exec =
            tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.execute(&pool)).await;
        let res = match exec {
            Err(_) => {
                pool.close().await;
                return Err(QueryExecError::Timeout);
            }
            Ok(Err(e)) => {
                pool.close().await;
                return Err(QueryExecError::Sql(e.to_string()));
            }
            Ok(Ok(r)) => r,
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        QueryExecOutcome {
            mode: "write",
            statement_kind,
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            rows_affected: Some(res.rows_affected()),
            truncated: false,
            duration_ms,
        }
    };

    pool.close().await;
    Ok(outcome)
}

/// Esito dell'archiviazione automatica di una DDL.
#[derive(Debug, Clone)]
pub struct DdlArchiveOutcome {
    pub note_id: Uuid,
    pub migration_filename: Option<String>,
    pub migration_abs_path: Option<String>,
}

/// Se la query e' DDL (CREATE/ALTER/DROP/TRUNCATE/RENAME), archivia
/// l'evento in due posti:
/// 1. Una nota nella Knowledge Base del progetto (`project_knowledge_notes`,
///    intent="database_migration", tag ["schema-change","ddl"]). Nessun
///    embedding qui: la nota e' comunque visibile nel pannello KB e
///    indicizzabile a posteriori.
/// 2. Un file migration versionato nella cartella migration del progetto
///    (path da `project_database_config.migration_path`, default
///    `nexus_migrations/` sotto la root). Il numero progressivo e' max+1
///    sui file `NNNN_*.sql` gia' presenti.
///
/// Idempotente sui retry: il caller deve invocarla SOLO dopo esecuzione
/// SQL riuscita (per non archiviare DDL fallite).
///
/// Errori vengono loggati come WARN ma non propagati: l'archiviazione e' un
/// "best effort". L'esecuzione SQL e' gia' avvenuta e il dato e' nel DB
/// applicativo.
pub async fn archive_ddl(
    db: &PgPool,
    project_id: Uuid,
    sql: &str,
    outcome: &QueryExecOutcome,
) -> Option<DdlArchiveOutcome> {
    if outcome.statement_kind != "ddl" {
        return None;
    }

    let trimmed = sql.trim();
    let timestamp = chrono::Utc::now();
    let action = first_token_upper(trimmed); // CREATE / ALTER / DROP / TRUNCATE / RENAME
    let object = second_meaningful_token_upper(trimmed); // TABLE / INDEX / VIEW / ...
    let target = third_meaningful_identifier(trimmed); // <nome>

    let title = match (action.as_str(), object.as_str(), target.as_deref()) {
        (a, "", _) => format!("DDL {} - {}", a, timestamp.format("%Y-%m-%d %H:%M")),
        (a, o, Some(t)) => format!("DDL {} {} {}", a, o, t),
        (a, o, None) => format!("DDL {} {}", a, o),
    };

    let body_md = format!(
        "**DDL eseguita dal pannello SQL** ({} righe modificate, {} ms)\n\n\
         ```sql\n{}\n```\n\n\
         _Archiviata automaticamente il {}_",
        outcome.rows_affected.unwrap_or(0),
        outcome.duration_ms,
        trimmed,
        timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
    );

    let note_id = Uuid::new_v4();
    let tags: Vec<String> = vec!["schema-change".to_string(), "ddl".to_string()];
    let file_paths: Vec<String> = Vec::new();

    let note_insert = sqlx::query(
        r#"
        INSERT INTO project_knowledge_notes
            (id, project_id, intent, title, body_md, status, qdrant_point_id, tags, file_paths)
        VALUES ($1, $2, 'database_migration', $3, $4, 'active', NULL, $5, $6)
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .bind(&title)
    .bind(&body_md)
    .bind(&tags)
    .bind(&file_paths)
    .execute(db)
    .await;

    if let Err(e) = note_insert {
        tracing::warn!(
            error = %e,
            %project_id,
            "archive_ddl: INSERT project_knowledge_notes fallito"
        );
        return None;
    }

    // Tag aggregati (best effort).
    for tag in &tags {
        let _ = sqlx::query(
            r#"
            INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
            VALUES ($1, $2, 1, NOW())
            ON CONFLICT (project_id, tag) DO UPDATE SET
                note_count = project_knowledge_tags.note_count + 1,
                last_used_at = NOW()
            "#,
        )
        .bind(project_id)
        .bind(tag)
        .execute(db)
        .await;
    }

    // File migration (best effort - se lo scrivi senza root, salta).
    let (mig_filename, mig_abs) = match write_migration_file(db, project_id, trimmed, &title).await
    {
        Ok((name, abs)) => (Some(name), Some(abs)),
        Err(e) => {
            tracing::warn!(
                error = %e,
                %project_id,
                "archive_ddl: scrittura file migration fallita"
            );
            (None, None)
        }
    };

    tracing::info!(
        %project_id,
        %note_id,
        migration_filename = mig_filename.as_deref().unwrap_or("(skip)"),
        "archive_ddl: DDL archiviata in KB"
    );

    Some(DdlArchiveOutcome {
        note_id,
        migration_filename: mig_filename,
        migration_abs_path: mig_abs,
    })
}

/// Scrive il file migration progressivo `<root>/<migration_path>/<NNNN>_<slug>.sql`.
///
/// - `migration_path` viene letto da `project_database_config.migration_path`
///   (relativo alla root). Default: `nexus_migrations/`.
/// - `<NNNN>` = max(numero progressivo gia' presente) + 1 (4 cifre, zero-padded).
/// - `<slug>` = primi 6 token sanificati del SQL.
///
/// Ritorna `(filename, absolute_path)` su successo. Errore se la root del
/// progetto non e' determinabile.
async fn write_migration_file(
    db: &PgPool,
    project_id: Uuid,
    sql: &str,
    title: &str,
) -> Result<(String, String), String> {
    // 1) Root del progetto.
    let root_row = sqlx::query("SELECT repository_root_path FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("query projects: {e}"))?
        .ok_or_else(|| "progetto non trovato".to_string())?;
    let root: Option<String> = root_row.try_get("repository_root_path").unwrap_or(None);
    let root = root
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "repository_root_path non configurato per il progetto".to_string())?;

    // 2) migration_path dal config (primary). Default nexus_migrations/.
    let cfg_row = sqlx::query(
        "SELECT migration_path FROM project_database_config \
         WHERE project_id = $1 AND is_primary = true \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("query project_database_config: {e}"))?;
    let migration_subdir: String = cfg_row
        .and_then(|r| r.try_get::<Option<String>, _>("migration_path").unwrap_or(None))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "nexus_migrations".to_string());

    let mig_dir = std::path::Path::new(&root).join(&migration_subdir);
    tokio::fs::create_dir_all(&mig_dir)
        .await
        .map_err(|e| format!("create_dir_all {}: {e}", mig_dir.display()))?;

    // 3) Calcola prossimo numero progressivo.
    let mut max_seen: u32 = 0;
    let mut entries = tokio::fs::read_dir(&mig_dir)
        .await
        .map_err(|e| format!("read_dir {}: {e}", mig_dir.display()))?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(fname) = entry.file_name().to_str() {
            if let Some(num_str) = fname.split('_').next() {
                if let Ok(n) = num_str.parse::<u32>() {
                    if n > max_seen {
                        max_seen = n;
                    }
                }
            }
        }
    }
    let next = max_seen + 1;
    let slug = slugify_sql(sql);
    let filename = format!("{:04}_{}.sql", next, slug);
    let abs_path = mig_dir.join(&filename);

    // 4) Scrivi il file con header commento.
    let header = format!(
        "-- {} (auto-archived by Nexus SQL panel)\n\
         -- Generated: {}\n\
         -- Project: {}\n\n",
        title,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        project_id,
    );
    let content = format!("{}{}\n", header, sql.trim_end());
    tokio::fs::write(&abs_path, content)
        .await
        .map_err(|e| format!("write {}: {e}", abs_path.display()))?;

    Ok((filename, abs_path.to_string_lossy().into_owned()))
}

fn first_token_upper(sql: &str) -> String {
    sql.split_whitespace()
        .next()
        .unwrap_or("")
        .to_uppercase()
}

fn second_meaningful_token_upper(sql: &str) -> String {
    // Salta IF EXISTS / IF NOT EXISTS dopo CREATE/DROP.
    let mut iter = sql.split_whitespace().skip(1);
    while let Some(tok) = iter.next() {
        let up = tok.to_uppercase();
        if up == "IF" || up == "NOT" || up == "EXISTS" || up == "OR" || up == "REPLACE" {
            continue;
        }
        return up;
    }
    String::new()
}

fn third_meaningful_identifier(sql: &str) -> Option<String> {
    let mut iter = sql.split_whitespace().skip(2);
    while let Some(tok) = iter.next() {
        let up = tok.to_uppercase();
        if up == "IF" || up == "NOT" || up == "EXISTS" || up == "OR" || up == "REPLACE" {
            continue;
        }
        // Strip trailing punctuation (es. "users(" o "schema.users,").
        let cleaned: String = tok
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

fn slugify_sql(sql: &str) -> String {
    let words: Vec<String> = sql
        .split_whitespace()
        .take(6)
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let slug = words.join("_");
    let slug: String = slug.chars().take(60).collect();
    if slug.is_empty() {
        "ddl".to_string()
    } else {
        slug
    }
}

/// Serializza un [`QueryExecOutcome`] nel formato JSON canonico usato sia dal
/// tool agente sia dall'endpoint REST.
pub fn outcome_to_json(outcome: &QueryExecOutcome) -> Value {
    match outcome.mode {
        "read" => json!({
            "ok": true,
            "mode": "read",
            "statement_kind": outcome.statement_kind,
            "columns": outcome.columns,
            "row_count": outcome.row_count,
            "rows": outcome.rows,
            "truncated": outcome.truncated,
            "duration_ms": outcome.duration_ms,
        }),
        _ => json!({
            "ok": true,
            "mode": "write",
            "statement_kind": outcome.statement_kind,
            "rows_affected": outcome.rows_affected.unwrap_or(0),
            "duration_ms": outcome.duration_ms,
            "hint": "Per leggere i dati inseriti usa una SELECT, oppure aggiungi RETURNING alla INSERT.",
        }),
    }
}
