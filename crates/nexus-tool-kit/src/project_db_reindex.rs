//! `project_db_reindex` — esegue REINDEX su tabella/indice del DB progetto.
//!
//! Operazione bloccante: avvertenza inclusa nell'output.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbReindexTool;

#[async_trait]
impl NexusToolHandler for ProjectDbReindexTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let target_type = args
            .get("target_type")
            .and_then(Value::as_str)
            .unwrap_or("table")
            .trim()
            .to_string();

        let target_name = args
            .get("target_name")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'target_name' obbligatorio".into()))?
            .trim()
            .to_string();

        if !target_name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            return Err(NexusToolError::BadInput(
                "Nome target contiene caratteri non validi".into(),
            ));
        }

        let sql = match target_type.as_str() {
            "table" => format!("REINDEX TABLE {}", target_name),
            "index" => format!("REINDEX INDEX {}", target_name),
            "database" => "REINDEX DATABASE CURRENT_DATABASE".to_string(),
            _ => {
                return Err(NexusToolError::BadInput(
                    "target_type deve essere 'table', 'index' o 'database'".into(),
                ))
            }
        };

        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let project_pool = db_helper::get_pool_for_project(&nexus_pool, ctx.project_id)
            .await
            .map_err(NexusToolError::BadInput)?;

        nexus_pool.close().await;

        let start = std::time::Instant::now();

        sqlx::query(&sql)
            .execute(&project_pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("REINDEX fallito: {}", e)))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        project_pool.close().await;

        Ok(json!({
            "ok": true,
            "operation": sql,
            "target_type": target_type,
            "target_name": target_name,
            "duration_ms": duration_ms,
            "warning": "REINDEX e' un'operazione bloccante. Le query sulla tabella/indice sono state bloccate durante l'esecuzione.",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["target_name"],
            "properties": {
                "target_type": {
                    "type": "string",
                    "enum": ["table", "index", "database"],
                    "description": "Tipo di target: 'table', 'index', o 'database'. Default: table"
                },
                "target_name": {
                    "type": "string",
                    "description": "Nome della tabella o dell'indice da reindicizzare"
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
        let s = ProjectDbReindexTool.safety();
        assert!(!s.read_only);
        assert!(s.network_egress);
    }

    #[test]
    fn test_input_requires_target_name() {
        let s = ProjectDbReindexTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "target_name"));
    }
}
