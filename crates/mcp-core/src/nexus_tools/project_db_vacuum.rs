//! `project_db_vacuum` — esegue VACUUM sul DB del progetto.
//!
//! Supporta VACUUM semplice, VACUUM ANALYZE, VACUUM FULL.
//! Opera sul DB del progetto (non su Nexus).

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbVacuumTool;

#[async_trait]
impl NexusToolHandler for ProjectDbVacuumTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let table = args.get("table").and_then(Value::as_str).map(|s| s.trim().to_string());
        let analyze = args.get("analyze").and_then(Value::as_bool).unwrap_or(true);
        let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);

        // Validazione nome tabella (no SQL injection)
        if let Some(ref t) = table {
            if !t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                return Err(NexusToolError::BadInput(
                    "Nome tabella contiene caratteri non validi".into(),
                ));
            }
        }

        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let project_pool = db_helper::get_pool_for_project(&nexus_pool, ctx.project_id)
            .await
            .map_err(|e| NexusToolError::BadInput(e))?;

        nexus_pool.close().await;

        // Costruisci query VACUUM (non parametrizzabile con bind)
        let mut sql = "VACUUM".to_string();
        if full {
            sql.push_str(" FULL");
        }
        if analyze {
            sql.push_str(" ANALYZE");
        }
        if let Some(ref t) = table {
            sql.push(' ');
            sql.push_str(t);
        }

        let start = std::time::Instant::now();

        // VACUUM non puo' essere eseguito in transazione, usiamo query diretta
        sqlx::query(&sql)
            .execute(&project_pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("VACUUM fallito: {}", e)))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        project_pool.close().await;

        Ok(json!({
            "ok": true,
            "operation": sql,
            "table": table,
            "full": full,
            "analyze": analyze,
            "duration_ms": duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "table": {
                    "type": "string",
                    "description": "Tabella specifica. Se omesso, opera su tutto il database."
                },
                "analyze": {
                    "type": "boolean",
                    "description": "Esegui VACUUM ANALYZE (aggiorna statistiche). Default: true"
                },
                "full": {
                    "type": "boolean",
                    "description": "Esegui VACUUM FULL (compatta, ma bloccante). Default: false"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
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
    fn test_safety() {
        let s = ProjectDbVacuumTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_execute_subproc);
        assert!(s.network_egress);
    }

    #[test]
    fn test_input_schema_no_required() {
        let s = ProjectDbVacuumTool.input_schema();
        assert!(s.get("required").is_none());
    }
}
