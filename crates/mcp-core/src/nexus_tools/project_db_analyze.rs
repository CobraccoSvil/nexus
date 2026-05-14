//! `project_db_analyze` — esegue ANALYZE sul DB del progetto.
//!
//! Aggiorna le statistiche del planner su una tabella o tutto il DB.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbAnalyzeTool;

#[async_trait]
impl NexusToolHandler for ProjectDbAnalyzeTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let table = args.get("table").and_then(Value::as_str).map(|s| s.trim().to_string());

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

        let sql = match &table {
            Some(t) => format!("ANALYZE {}", t),
            None => "ANALYZE".to_string(),
        };

        let start = std::time::Instant::now();

        sqlx::query(&sql)
            .execute(&project_pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("ANALYZE fallito: {}", e)))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        project_pool.close().await;

        Ok(json!({
            "ok": true,
            "operation": sql,
            "table": table,
            "duration_ms": duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "table": {
                    "type": "string",
                    "description": "Tabella specifica da analizzare. Se omesso, analizza tutto il database."
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
        let s = ProjectDbAnalyzeTool.safety();
        assert!(!s.read_only);
        assert!(s.network_egress);
    }
}
