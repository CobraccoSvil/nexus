//! `project_db_schema` — inspect schema del DB del progetto.
//!
//! Equivalente a `db_schema_inspect` ma usa la connessione configurata in
//! `project_database_config` invece di `DATABASE_URL` di Nexus. Restituisce
//! tabelle + colonne (nome, tipo, nullable) per uno schema (default `public`)
//! o per una singola tabella se `table` e' specificato.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectDbSchemaTool;

#[async_trait]
impl NexusToolHandler for ProjectDbSchemaTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let schema = args
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();
        let table_filter = args.get("table").and_then(Value::as_str).map(String::from);

        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let project_pool = db_helper::get_pool_for_project(&nexus_pool, ctx.project_id)
            .await
            .map_err(|e| {
                NexusToolError::BadInput(format!("apertura DB progetto fallita: {}", e))
            })?;

        nexus_pool.close().await;

        let mut tables_q = sqlx::query(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = $1 AND table_type='BASE TABLE'
             ORDER BY table_name",
        )
        .bind(&schema)
        .fetch_all(&project_pool)
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
                "SELECT column_name, data_type, is_nullable, column_default
                 FROM information_schema.columns
                 WHERE table_schema = $1 AND table_name = $2
                 ORDER BY ordinal_position",
            )
            .bind(&schema)
            .bind(&table_name)
            .fetch_all(&project_pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("query cols failed: {}", e)))?;

            let cols_json: Vec<Value> = cols
                .iter()
                .map(|c| {
                    let name: String = c.try_get("column_name").unwrap_or_default();
                    let dtype: String = c.try_get("data_type").unwrap_or_default();
                    let nullable: String = c.try_get("is_nullable").unwrap_or_default();
                    let default: Option<String> = c.try_get("column_default").ok();
                    json!({
                        "name": name,
                        "type": dtype,
                        "nullable": nullable == "YES",
                        "default": default,
                    })
                })
                .collect();

            // Conta righe (best-effort, ignora errore)
            let row_count: Option<i64> = sqlx::query_scalar(&format!(
                "SELECT reltuples::bigint AS estimate
                 FROM pg_class
                 WHERE oid = '\"{}\".\"{}\"'::regclass",
                schema.replace('"', "\"\""),
                table_name.replace('"', "\"\"")
            ))
            .fetch_optional(&project_pool)
            .await
            .ok()
            .flatten();

            tables_out.push(json!({
                "name": table_name,
                "columns": cols_json,
                "estimated_row_count": row_count,
            }));
        }

        project_pool.close().await;

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
                "schema": {
                    "type": "string",
                    "description": "Schema PostgreSQL da ispezionare. Default: public"
                },
                "table": {
                    "type": "string",
                    "description": "Se specificato, ritorna solo questa tabella"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_readonly_network() {
        let s = ProjectDbSchemaTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_write_filesystem);
    }

    #[test]
    fn test_input_schema_optional_table() {
        let s = ProjectDbSchemaTool.input_schema();
        assert!(s["properties"]["table"].is_object());
        assert!(s["properties"]["schema"].is_object());
    }
}
