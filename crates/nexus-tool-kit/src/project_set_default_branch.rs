//! `project_set_default_branch` — aggiorna il branch predefinito di un progetto.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectSetDefaultBranchTool;

#[async_trait]
impl NexusToolHandler for ProjectSetDefaultBranchTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let branch = args
            .get("branch")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'branch' obbligatorio".into()))?
            .trim()
            .to_string();

        if branch.is_empty() {
            return Err(NexusToolError::BadInput("Branch vuoto".into()));
        }

        let pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let result = sqlx::query("UPDATE projects SET default_branch = $1 WHERE id = $2")
            .bind(&branch)
            .bind(ctx.project_id)
            .execute(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("update branch: {}", e)))?;

        pool.close().await;

        if result.rows_affected() == 0 {
            return Err(NexusToolError::BadInput("Progetto non trovato".into()));
        }

        Ok(json!({
            "ok": true,
            "project_id": ctx.project_id.to_string(),
            "default_branch": branch,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["branch"],
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Nome del branch predefinito (es. 'main', 'develop')"
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
        let s = ProjectSetDefaultBranchTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_write_filesystem);
    }

    #[test]
    fn test_input_schema_requires_branch() {
        let s = ProjectSetDefaultBranchTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "branch"));
    }
}
