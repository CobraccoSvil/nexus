//! `database::db_schema_inspect` — introspezione schema PostgreSQL.
//!
//! Usa la stessa connessione globale di mcp-core (variabile d'ambiente
//! `DATABASE_URL`) per leggere `information_schema.tables` e `.columns`
//! filtrate per schema (default `public`).
//!
//! Output: `{schema, tables: [{name, columns: [{name, type, nullable}]}]}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

pub struct DbSchemaInspectTool;

#[async_trait]
impl NexusToolHandler for DbSchemaInspectTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = args
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table_filter = args
            .get("table")
            .and_then(Value::as_str)
            .map(String::from);

        let db_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => {
                return Ok(json!({
                    "ok": false,
                    "error": "DATABASE_URL not set in environment",
                }));
            }
        };

        let pool = match PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(&db_url)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({
                    "ok": false,
                    "error": format!("db connect failed: {}", e),
                }));
            }
        };

        // Lista tabelle
        let mut tables_q = sqlx::query(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = $1 AND table_type='BASE TABLE'
             ORDER BY table_name",
        )
        .bind(&schema)
        .fetch_all(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("query tables failed: {}", e)))?;

        if let Some(f) = &table_filter {
            tables_q.retain(|r| {
                r.try_get::<String, _>("table_name")
                    .map(|n| n == *f)
                    .unwrap_or(false)
            });
        }

        let mut tables_out: Vec<Value> = Vec::with_capacity(tables_q.len());
        for row in tables_q {
            let table_name: String = row
                .try_get("table_name")
                .map_err(|e| NexusToolError::BadInput(format!("row decode: {}", e)))?;
            let cols = sqlx::query(
                "SELECT column_name, data_type, is_nullable
                 FROM information_schema.columns
                 WHERE table_schema = $1 AND table_name = $2
                 ORDER BY ordinal_position",
            )
            .bind(&schema)
            .bind(&table_name)
            .fetch_all(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("query cols failed: {}", e)))?;
            let cols_json: Vec<Value> = cols
                .iter()
                .map(|c| {
                    let name: String = c.try_get("column_name").unwrap_or_default();
                    let dtype: String = c.try_get("data_type").unwrap_or_default();
                    let nullable: String = c.try_get("is_nullable").unwrap_or_default();
                    json!({
                        "name": name,
                        "type": dtype,
                        "nullable": nullable == "YES",
                    })
                })
                .collect();
            tables_out.push(json!({
                "name": table_name,
                "columns": cols_json,
            }));
        }

        pool.close().await;

        Ok(json!({
            "ok": true,
            "schema": schema,
            "table_count": tables_out.len(),
            "tables": tables_out,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "schema": {"type": "string", "default": "public"},
                "table": {"type": "string", "description": "Filtra una sola tabella"}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_readonly_network() {
        let s = DbSchemaInspectTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_structure() {
        let s = DbSchemaInspectTool.input_schema();
        assert!(s["properties"]["schema"].is_object());
    }
}
