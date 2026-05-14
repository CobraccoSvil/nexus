//! `project_db_query` — esegue query SELECT/EXPLAIN/SHOW sul DB del progetto.
//!
//! Read-only stretto: la query passa per whitelist (deve iniziare con SELECT,
//! WITH ... SELECT, EXPLAIN o SHOW) e non puo' contenere DDL/DML. Limit di
//! sicurezza a 100 righe nel risultato. Il pool e' aperto sulla
//! `connection_secret` letta da `project_database_config`, non sul pool Nexus.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::{Column, Row, TypeInfo};

pub struct ProjectDbQueryTool;

const MAX_ROWS: usize = 100;

/// Whitelist: prima keyword della query (ignorando whitespace e commenti).
fn first_keyword(sql: &str) -> Option<String> {
    // Rimuove commenti SQL standard "--..." su singola riga
    let cleaned: String = sql
        .lines()
        .map(|l| l.split_once("--").map(|x| x.0).unwrap_or(l))
        .collect::<Vec<_>>()
        .join(" ");

    cleaned
        .split_whitespace()
        .next()
        .map(|s| s.to_uppercase())
}

fn is_read_only_query(sql: &str) -> bool {
    let kw = match first_keyword(sql) {
        Some(k) => k,
        None => return false,
    };
    matches!(
        kw.as_str(),
        "SELECT" | "WITH" | "EXPLAIN" | "SHOW" | "VALUES" | "TABLE"
    )
}

#[async_trait]
impl NexusToolHandler for ProjectDbQueryTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let sql = args
            .get("sql")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'sql' obbligatorio".into()))?
            .trim()
            .to_string();

        if sql.is_empty() {
            return Err(NexusToolError::BadInput("Query SQL vuota".into()));
        }

        if !is_read_only_query(&sql) {
            return Err(NexusToolError::BadInput(format!(
                "Solo query read-only consentite (SELECT/WITH/EXPLAIN/SHOW/VALUES/TABLE). Prima keyword: '{}'",
                first_keyword(&sql).unwrap_or_else(|| "<vuota>".to_string())
            )));
        }

        if db_helper::contains_ddl_statement(&sql) {
            return Err(NexusToolError::BadInput(
                "La query contiene istruzioni DDL. Usa project_db_create_migration per modifiche schema.".into(),
            ));
        }

        // Apri pool su Nexus per leggere project_database_config, poi pool su DB progetto
        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let project_pool = db_helper::get_pool_for_project(&nexus_pool, ctx.project_id)
            .await
            .map_err(|e| {
                NexusToolError::BadInput(format!("apertura DB progetto fallita: {}", e))
            })?;

        nexus_pool.close().await;

        // Esegui la query con LIMIT implicito tramite fetch_all + truncate
        let rows = sqlx::query(&sql)
            .fetch_all(&project_pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("query failed: {}", e)))?;

        let total_rows = rows.len();
        let truncated = total_rows > MAX_ROWS;
        let limited_rows: Vec<_> = rows.into_iter().take(MAX_ROWS).collect();

        // Estrae nomi colonne dalla prima riga (se presente)
        let columns: Vec<String> = if let Some(first) = limited_rows.first() {
            first
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect()
        } else {
            Vec::new()
        };

        // Converte ogni riga in oggetto JSON best-effort.
        let mut out_rows: Vec<Value> = Vec::with_capacity(limited_rows.len());
        for row in &limited_rows {
            let mut obj = serde_json::Map::new();
            for col in row.columns() {
                let name = col.name();
                let type_name = col.type_info().name();
                let value = decode_column(row, name, type_name);
                obj.insert(name.to_string(), value);
            }
            out_rows.push(Value::Object(obj));
        }

        project_pool.close().await;

        Ok(json!({
            "ok": true,
            "columns": columns,
            "row_count": out_rows.len(),
            "total_rows_available": total_rows,
            "truncated": truncated,
            "max_rows": MAX_ROWS,
            "rows": out_rows,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["sql"],
            "properties": {
                "sql": {
                    "type": "string",
                    "description": "Query SQL read-only (SELECT/WITH/EXPLAIN/SHOW/VALUES/TABLE). DDL e DML scrittura sono rifiutati. Limit 100 righe."
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: true,
        }
    }
}

/// Decodifica best-effort di una colonna in JSON. Supporta i tipi Postgres piu'
/// comuni; fallback a stringa per tipi non riconosciuti.
fn decode_column(row: &sqlx::postgres::PgRow, name: &str, type_name: &str) -> Value {
    match type_name {
        "INT2" | "INT4" => row
            .try_get::<Option<i32>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v))
            .unwrap_or(Value::Null),
        "INT8" => row
            .try_get::<Option<i64>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v))
            .unwrap_or(Value::Null),
        "FLOAT4" => row
            .try_get::<Option<f32>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v as f64))
            .unwrap_or(Value::Null),
        "FLOAT8" | "NUMERIC" => row
            .try_get::<Option<f64>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v))
            .unwrap_or(Value::Null),
        "BOOL" => row
            .try_get::<Option<bool>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v))
            .unwrap_or(Value::Null),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "CHAR" => row
            .try_get::<Option<String>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v))
            .unwrap_or(Value::Null),
        "UUID" => row
            .try_get::<Option<uuid::Uuid>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v.to_string()))
            .unwrap_or(Value::Null),
        "TIMESTAMPTZ" | "TIMESTAMP" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v.to_rfc3339()))
            .unwrap_or_else(|| {
                row.try_get::<Option<chrono::NaiveDateTime>, _>(name)
                    .ok()
                    .flatten()
                    .map(|v| Value::from(v.to_string()))
                    .unwrap_or(Value::Null)
            }),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(name)
            .ok()
            .flatten()
            .map(|v| Value::from(v.to_string()))
            .unwrap_or(Value::Null),
        "JSON" | "JSONB" => row
            .try_get::<Option<Value>, _>(name)
            .ok()
            .flatten()
            .unwrap_or(Value::Null),
        _ => {
            // Fallback: prova stringa, altrimenti null
            row.try_get::<Option<String>, _>(name)
                .ok()
                .flatten()
                .map(|v| Value::from(v))
                .unwrap_or(Value::Null)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_keyword_strips_comments() {
        assert_eq!(first_keyword("-- comment\nSELECT 1"), Some("SELECT".into()));
        assert_eq!(first_keyword("  SELECT 1"), Some("SELECT".into()));
    }

    #[test]
    fn test_read_only_accepts() {
        assert!(is_read_only_query("SELECT 1"));
        assert!(is_read_only_query("with cte as (select 1) select * from cte"));
        assert!(is_read_only_query("EXPLAIN SELECT 1"));
        assert!(is_read_only_query("SHOW search_path"));
        assert!(is_read_only_query("VALUES (1)"));
        assert!(is_read_only_query("TABLE users"));
    }

    #[test]
    fn test_read_only_rejects_writes() {
        assert!(!is_read_only_query("INSERT INTO u VALUES (1)"));
        assert!(!is_read_only_query("UPDATE u SET x=1"));
        assert!(!is_read_only_query("DELETE FROM u"));
        assert!(!is_read_only_query("DROP TABLE u"));
        assert!(!is_read_only_query("ALTER TABLE u ADD COLUMN x int"));
        assert!(!is_read_only_query("TRUNCATE u"));
        assert!(!is_read_only_query("CREATE TABLE u (x int)"));
    }

    #[test]
    fn test_safety_readonly_with_network() {
        let s = ProjectDbQueryTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_write_filesystem);
    }

    #[test]
    fn test_input_schema_requires_sql() {
        let s = ProjectDbQueryTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "sql"));
    }
}
