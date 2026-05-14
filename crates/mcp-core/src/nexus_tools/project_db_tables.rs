//! `project_db_tables` — lista tabelle del DB del progetto.
//!
//! Versione sintetica di `project_db_schema`: ritorna solo nome tabella +
//! row count stimato + dimensione su disco. Utile per quick-look dello stato
//! del DB senza il costo di leggere tutte le colonne.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectDbTablesTool;

#[async_trait]
impl NexusToolHandler for ProjectDbTablesTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let schema = args
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or("public")
            .to_string();

        let include_views = args
            .get("include_views")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let project_pool = db_helper::get_pool_for_project(&nexus_pool, ctx.project_id)
            .await
            .map_err(|e| {
                NexusToolError::BadInput(format!("apertura DB progetto fallita: {}", e))
            })?;

        nexus_pool.close().await;

        let type_filter = if include_views {
            "('BASE TABLE','VIEW')"
        } else {
            "('BASE TABLE')"
        };

        let query = format!(
            "SELECT
                t.table_name,
                t.table_type,
                COALESCE(c.reltuples::bigint, 0) AS row_estimate,
                COALESCE(pg_total_relation_size(c.oid), 0) AS size_bytes
             FROM information_schema.tables t
             LEFT JOIN pg_class c
                ON c.relname = t.table_name
                AND c.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)
             WHERE t.table_schema = $1 AND t.table_type IN {}
             ORDER BY t.table_name",
            type_filter
        );

        let rows = sqlx::query(&query)
            .bind(&schema)
            .fetch_all(&project_pool)
            .await
            .map_err(|e| {
                NexusToolError::BadInput(format!("query tables failed: {}", e))
            })?;

        let tables: Vec<Value> = rows
            .iter()
            .map(|r| {
                let name: String = r.try_get("table_name").unwrap_or_default();
                let ttype: String = r.try_get("table_type").unwrap_or_default();
                let row_est: i64 = r.try_get("row_estimate").unwrap_or(0);
                let size_bytes: i64 = r.try_get("size_bytes").unwrap_or(0);
                json!({
                    "name": name,
                    "type": if ttype == "VIEW" { "view" } else { "table" },
                    "estimated_row_count": row_est,
                    "size_bytes": size_bytes,
                    "size_pretty": format_size(size_bytes),
                })
            })
            .collect();

        project_pool.close().await;

        Ok(json!({
            "ok": true,
            "schema": schema,
            "count": tables.len(),
            "tables": tables,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "schema": {
                    "type": "string",
                    "description": "Schema PostgreSQL. Default: public"
                },
                "include_views": {
                    "type": "boolean",
                    "description": "Includi anche le viste. Default: false"
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

fn format_size(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.00 KB");
        assert_eq!(format_size(1024 * 1024 * 5), "5.00 MB");
        assert_eq!(format_size(1024_i64 * 1024 * 1024 * 3), "3.00 GB");
    }

    #[test]
    fn test_safety_readonly_network() {
        let s = ProjectDbTablesTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
    }

    #[test]
    fn test_input_schema_optional_params() {
        let s = ProjectDbTablesTool.input_schema();
        assert!(s["properties"]["schema"].is_object());
        assert!(s["properties"]["include_views"].is_object());
    }
}
