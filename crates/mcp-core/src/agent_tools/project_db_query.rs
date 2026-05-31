//! Tool agente per gestione runtime del DB applicativo del progetto.
//!
//! Bug osservato 31/05/2026: l'utente chiede "inserisci un utente con email X"
//! ma il modello (Vertex gemini-2.5-pro) tenta `psql` (non installato nel WSL
//! host) e si blocca. Non esisteva un tool per eseguire SQL ad-hoc sul DB
//! applicativo del progetto: i tool `project_db_*` coprono solo le migration
//! versionate, non query interattive.
//!
//! Questo modulo espone 3 tool builtin (thin wrapper sopra
//! `crate::project_db::exec` per non duplicare la logica con l'endpoint
//! REST `POST /api/projects/:id/db/query`):
//!   - `nexus_db_query`    : esegue SQL arbitrario (SELECT/INSERT/UPDATE/DELETE/DDL)
//!   - `nexus_db_tables`   : lista le tabelle dello schema public
//!   - `nexus_db_describe` : colonne/tipi/vincoli/indici di una tabella
//!
//! Sicurezza (regola E CLAUDE.md - isolamento progetti):
//!   - La connessione viene SEMPRE risolta da `project_database_config` del
//!     progetto attivo (via `crate::project_db::exec::resolve_project_conn`).
//!   - Guard-rail anti-contaminazione verso il DB infrastruttura Nexus.
//!   - Limiti: timeout query 30s, max 1000 righe ritornate.

use serde_json::{json, Value};
use sqlx::Row;

use super::AgentToolContext;
use crate::project_db::exec::{
    self, execute_query, open_pool, outcome_to_json, resolve_project_conn, QueryExecError,
};

/// Tool `nexus_db_query`. Thin wrapper sopra `crate::project_db::exec::execute_query`.
pub(super) async fn tool_nexus_db_query(ctx: &AgentToolContext, input: &Value) -> String {
    let sql = match input.get("sql").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return json!({"error": "Parametro 'sql' obbligatorio (stringa non vuota)."}).to_string();
        }
    };

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
        .map(|n| n as usize);

    match execute_query(&ctx.db, ctx.project_id, &sql, &params, max_rows).await {
        Ok(outcome) => outcome_to_json(&outcome).to_string(),
        Err(e) => match e {
            QueryExecError::ConnectionError(m) => json!({"error": m}).to_string(),
            QueryExecError::Timeout => {
                json!({"error": e.message()}).to_string()
            }
            QueryExecError::Sql(_) => json!({
                "error": e.message(),
                "sql_excerpt": sql.chars().take(200).collect::<String>(),
            })
            .to_string(),
        },
    }
}

/// Tool `nexus_db_tables`: lista tabelle + righe stimate dello schema.
pub(super) async fn tool_nexus_db_tables(ctx: &AgentToolContext, input: &Value) -> String {
    let schema = input
        .get("schema")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "public".to_string());

    let conn = match resolve_project_conn(&ctx.db, ctx.project_id).await {
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

    let conn = match resolve_project_conn(&ctx.db, ctx.project_id).await {
        Ok(c) => c,
        Err(e) => return json!({"error": e}).to_string(),
    };
    let pool = match open_pool(&conn).await {
        Ok(p) => p,
        Err(e) => return json!({"error": e}).to_string(),
    };

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
        // Costanti esposte per documentazione (riusate via exec module).
        "_limits": {
            "max_rows": exec::MAX_ROWS,
            "query_timeout_secs": exec::QUERY_TIMEOUT_SECS,
            "max_cell_chars": exec::MAX_CELL_CHARS,
        }
    })
    .to_string()
}
