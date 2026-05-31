//! Tool per gestione runtime del DB applicativo del progetto.
//!
//! Bug osservato 31/05/2026: l'utente chiede "inserisci un utente con email X"
//! ma il modello (Vertex gemini-2.5-pro) tenta `psql` (non installato nel WSL
//! host) e si blocca. Non esisteva un tool per eseguire SQL ad-hoc sul DB
//! applicativo del progetto: i tool `project_db_*` coprono solo le migration
//! versionate, non query interattive.
//!
//! Questo modulo espone 3 tool builtin:
//!   - `nexus_db_query`    : esegue SQL arbitrario (SELECT/INSERT/UPDATE/DELETE/DDL)
//!   - `nexus_db_tables`   : lista le tabelle dello schema public
//!   - `nexus_db_describe` : colonne/tipi/vincoli/indici di una tabella
//!
//! Sicurezza (regola E CLAUDE.md - isolamento progetti):
//!   - La connessione viene SEMPRE risolta da `project_database_config` del
//!     progetto attivo (via `load_primary_config`). Mai una connessione
//!     arbitraria passata dal modello.
//!   - Guard-rail anti-contaminazione: se la connection string punta al DB
//!     infrastruttura Nexus (db `nexus` sulla porta 5433), la query viene
//!     RIFIUTATA. Un tool applicativo non deve mai toccare il DB di sistema.
//!   - Limiti: timeout query 30s, max 1000 righe ritornate.

use serde_json::{json, Value};
use sqlx::{Column, Row, TypeInfo};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

use super::AgentToolContext;

const QUERY_TIMEOUT_SECS: u64 = 30;
const MAX_ROWS: usize = 1000;
const MAX_CELL_CHARS: usize = 20_000;

/// Risolve la connection string del DB primario del progetto, applicando il
/// guard-rail anti-contaminazione verso il DB Nexus.
///
/// Legge `connection_secret` (bytea con la URL raw, scritta da
/// ensure_project_db_url / project_db_set_connection) dal pool Nexus (ctx.db).
async fn resolve_project_conn(ctx: &AgentToolContext) -> Result<String, String> {
    let row = sqlx::query(
        "SELECT connection_secret FROM project_database_config \
         WHERE project_id = $1 AND is_primary = true \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(ctx.project_id)
    .fetch_optional(&*ctx.db)
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
        && (lower.contains(":5433") || lower.contains("postgres-nexus") || lower.contains("ideai-postgres-nexus"));
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
async fn open_pool(conn: &str) -> Result<sqlx::PgPool, String> {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(conn)
        .await
        .map_err(|e| format!("connessione DB progetto fallita: {e}"))
}

/// Converte una cella di PgRow in serde_json::Value in base al tipo Postgres.
/// I tipi non gestiti esplicitamente ricadono su String; se anche quella
/// fallisce, ritorna null con un marcatore di tipo.
fn cell_to_json(row: &sqlx::postgres::PgRow, idx: usize, type_name: &str) -> Value {
    macro_rules! try_as {
        ($t:ty) => {
            row.try_get::<Option<$t>, _>(idx).ok().map(|o| o.map(Value::from))
        };
    }
    let val: Option<Option<Value>> = match type_name {
        "BOOL" => row.try_get::<Option<bool>, _>(idx).ok().map(|o| o.map(Value::from)),
        "INT2" => row.try_get::<Option<i16>, _>(idx).ok().map(|o| o.map(|v| Value::from(v as i64))),
        "INT4" => row.try_get::<Option<i32>, _>(idx).ok().map(|o| o.map(|v| Value::from(v as i64))),
        "INT8" => row.try_get::<Option<i64>, _>(idx).ok().map(|o| o.map(Value::from)),
        "FLOAT4" => row.try_get::<Option<f32>, _>(idx).ok().map(|o| o.map(|v| Value::from(v as f64))),
        "FLOAT8" => try_as!(f64),
        // NUMERIC: la feature bigdecimal/decimal di sqlx non e' attiva, quindi
        // proviamo f64 (perdita precisione accettabile per display); se il
        // Decode fallisce a runtime ricade nel fallback finale (marcatore).
        "NUMERIC" => try_as!(f64),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "CHAR" | "CITEXT" => try_as!(String),
        "UUID" => row.try_get::<Option<uuid::Uuid>, _>(idx).ok().map(|o| o.map(|v| Value::from(v.to_string()))),
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
        Some(None) => Value::Null, // colonna NULL
        None => {
            // Tipo non gestito: prova String, poi marcatore.
            match row.try_get::<Option<String>, _>(idx) {
                Ok(Some(s)) => clamp_cell(Value::from(s)),
                Ok(None) => Value::Null,
                Err(_) => Value::from(format!("<tipo non serializzabile: {type_name}>")),
            }
        }
    }
}

/// Tronca le celle stringa enormi per non saturare il context window.
fn clamp_cell(v: Value) -> Value {
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

/// Determina se la query e' di sola lettura (SELECT/WITH...SELECT/SHOW/EXPLAIN).
fn is_read_only(sql: &str) -> bool {
    let t = sql.trim_start().to_lowercase();
    t.starts_with("select") || t.starts_with("show") || t.starts_with("explain") || t.starts_with("with")
}

/// Tool `nexus_db_query`.
pub(super) async fn tool_nexus_db_query(ctx: &AgentToolContext, input: &Value) -> String {
    let sql = match input.get("sql").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return json!({"error": "Parametro 'sql' obbligatorio (stringa non vuota)."}).to_string(),
    };

    // Parametri opzionali: array JSON. Ogni valore viene bindato come TEXT
    // (NULL -> bind null). Il modello usa cast espliciti nel SQL quando serve
    // un tipo non-testo: es. INSERT INTO t(qty) VALUES ($1::int).
    let params: Vec<Option<String>> = match input.get("params") {
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|v| match v {
                Value::Null => None,
                Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            })
            .collect(),
        _ => Vec::new(),
    };

    let max_rows = input
        .get("max_rows")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).min(MAX_ROWS))
        .unwrap_or(MAX_ROWS);

    let conn = match resolve_project_conn(ctx).await {
        Ok(c) => c,
        Err(e) => return json!({"error": e}).to_string(),
    };
    let pool = match open_pool(&conn).await {
        Ok(p) => p,
        Err(e) => return json!({"error": e}).to_string(),
    };

    let read_only = is_read_only(&sql);

    // Costruisce la query con i bind.
    let mut q = sqlx::query(&sql);
    for p in &params {
        q = q.bind(p.clone());
    }

    let started = std::time::Instant::now();

    if read_only {
        // fetch_all con timeout
        let fetch = tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.fetch_all(&pool)).await;
        let rows = match fetch {
            Err(_) => {
                pool.close().await;
                return json!({"error": format!("timeout query dopo {QUERY_TIMEOUT_SECS}s")}).to_string();
            }
            Ok(Err(e)) => {
                pool.close().await;
                return json!({"error": format!("errore SQL: {e}"), "sql_excerpt": sql.chars().take(200).collect::<String>()}).to_string();
            }
            Ok(Ok(r)) => r,
        };
        let duration_ms = started.elapsed().as_millis() as u64;

        let truncated = rows.len() > max_rows;
        let slice = &rows[..rows.len().min(max_rows)];

        // Estrai nomi colonne dalla prima riga (se presente).
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

        pool.close().await;
        json!({
            "ok": true,
            "mode": "read",
            "columns": columns,
            "row_count": out_rows.len(),
            "rows": out_rows,
            "truncated": truncated,
            "duration_ms": duration_ms,
        })
        .to_string()
    } else {
        // DML/DDL: execute con timeout, ritorna rows_affected.
        let exec = tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.execute(&pool)).await;
        let res = match exec {
            Err(_) => {
                pool.close().await;
                return json!({"error": format!("timeout query dopo {QUERY_TIMEOUT_SECS}s")}).to_string();
            }
            Ok(Err(e)) => {
                pool.close().await;
                return json!({"error": format!("errore SQL: {e}"), "sql_excerpt": sql.chars().take(200).collect::<String>()}).to_string();
            }
            Ok(Ok(r)) => r,
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        pool.close().await;
        json!({
            "ok": true,
            "mode": "write",
            "rows_affected": res.rows_affected(),
            "duration_ms": duration_ms,
            "hint": "Per leggere i dati inseriti usa nexus_db_query con una SELECT, oppure aggiungi RETURNING alla INSERT.",
        })
        .to_string()
    }
}

/// Tool `nexus_db_tables`: lista tabelle + righe stimate dello schema public.
pub(super) async fn tool_nexus_db_tables(ctx: &AgentToolContext, input: &Value) -> String {
    let schema = input
        .get("schema")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "public".to_string());

    let conn = match resolve_project_conn(ctx).await {
        Ok(c) => c,
        Err(e) => return json!({"error": e}).to_string(),
    };
    let pool = match open_pool(&conn).await {
        Ok(p) => p,
        Err(e) => return json!({"error": e}).to_string(),
    };

    let rows = sqlx::query(
        r#"SELECT t.table_name,
                  COALESCE(c.reltuples::bigint, 0) AS est_rows
           FROM information_schema.tables t
           LEFT JOIN pg_class c ON c.relname = t.table_name
           WHERE t.table_schema = $1 AND t.table_type = 'BASE TABLE'
           ORDER BY t.table_name"#,
    )
    .bind(&schema)
    .fetch_all(&pool)
    .await;

    let result = match rows {
        Ok(r) => {
            let tables: Vec<Value> = r
                .iter()
                .map(|row| {
                    json!({
                        "name": row.try_get::<String, _>("table_name").unwrap_or_default(),
                        "estimated_rows": row.try_get::<i64, _>("est_rows").unwrap_or(0),
                    })
                })
                .collect();
            json!({"ok": true, "schema": schema, "table_count": tables.len(), "tables": tables})
        }
        Err(e) => json!({"error": format!("errore listing tabelle: {e}")}),
    };
    pool.close().await;
    result.to_string()
}

/// Tool `nexus_db_describe`: colonne, tipi, vincoli e indici di una tabella.
pub(super) async fn tool_nexus_db_describe(ctx: &AgentToolContext, input: &Value) -> String {
    let table = match input.get("table").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return json!({"error": "Parametro 'table' obbligatorio."}).to_string(),
    };
    let schema = input
        .get("schema")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "public".to_string());

    let conn = match resolve_project_conn(ctx).await {
        Ok(c) => c,
        Err(e) => return json!({"error": e}).to_string(),
    };
    let pool = match open_pool(&conn).await {
        Ok(p) => p,
        Err(e) => return json!({"error": e}).to_string(),
    };

    // Colonne
    let col_rows = sqlx::query(
        r#"SELECT column_name, data_type, is_nullable, column_default,
                  character_maximum_length
           FROM information_schema.columns
           WHERE table_schema = $1 AND table_name = $2
           ORDER BY ordinal_position"#,
    )
    .bind(&schema)
    .bind(&table)
    .fetch_all(&pool)
    .await;

    let columns: Vec<Value> = match col_rows {
        Ok(r) => r
            .iter()
            .map(|row| {
                json!({
                    "name": row.try_get::<String, _>("column_name").unwrap_or_default(),
                    "type": row.try_get::<String, _>("data_type").unwrap_or_default(),
                    "nullable": row.try_get::<String, _>("is_nullable").map(|s| s == "YES").unwrap_or(true),
                    "default": row.try_get::<Option<String>, _>("column_default").unwrap_or(None),
                    "max_length": row.try_get::<Option<i32>, _>("character_maximum_length").unwrap_or(None),
                })
            })
            .collect(),
        Err(e) => {
            pool.close().await;
            return json!({"error": format!("errore descrizione colonne: {e}")}).to_string();
        }
    };

    if columns.is_empty() {
        pool.close().await;
        return json!({
            "error": format!("Tabella '{schema}.{table}' non trovata o senza colonne."),
            "hint": "Usa nexus_db_tables per vedere le tabelle disponibili."
        })
        .to_string();
    }

    // Indici (incluse PK/unique)
    let idx_rows = sqlx::query(
        r#"SELECT indexname, indexdef
           FROM pg_indexes
           WHERE schemaname = $1 AND tablename = $2
           ORDER BY indexname"#,
    )
    .bind(&schema)
    .bind(&table)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let indexes: Vec<Value> = idx_rows
        .iter()
        .map(|row| {
            json!({
                "name": row.try_get::<String, _>("indexname").unwrap_or_default(),
                "definition": row.try_get::<String, _>("indexdef").unwrap_or_default(),
            })
        })
        .collect();

    pool.close().await;
    json!({
        "ok": true,
        "schema": schema,
        "table": table,
        "columns": columns,
        "indexes": indexes,
    })
    .to_string()
}
