//! `project_delete` — soft-delete di un progetto dal DB.
//!
//! Rimuove il progetto e tutte le tabelle dipendenti (CASCADE su FK).
//! Non cancella i file dal disco. Richiede conferma esplicita.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub struct ProjectDeleteTool;

#[async_trait]
impl NexusToolHandler for ProjectDeleteTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let project_id_str = args
            .get("project_id")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'project_id' obbligatorio".into()))?;

        let target_id = Uuid::parse_str(project_id_str)
            .map_err(|_| NexusToolError::BadInput("project_id non valido".into()))?;

        let confirm = args
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if !confirm {
            return Err(NexusToolError::BadInput(
                "Operazione distruttiva: specifica confirm:true per procedere. I file su disco NON vengono cancellati.".into(),
            ));
        }

        let pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        // Verifica che il progetto esista e l'utente ne sia owner
        let row = sqlx::query(
            "SELECT name, owner_user_id FROM projects WHERE id = $1",
        )
        .bind(target_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("lookup progetto: {}", e)))?;

        let row = match row {
            Some(r) => r,
            None => {
                pool.close().await;
                return Err(NexusToolError::BadInput(format!(
                    "Progetto '{}' non trovato",
                    project_id_str
                )));
            }
        };

        let project_name: String = row.try_get("name").unwrap_or_default();
        let owner_id: Uuid = row.try_get("owner_user_id").unwrap_or_default();

        if owner_id != ctx.user_id {
            pool.close().await;
            return Err(NexusToolError::BadInput(
                "Solo il proprietario del progetto puo' eliminarlo".into(),
            ));
        }

        // Cancella il progetto (CASCADE elimina tabelle dipendenti)
        let result = sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(target_id)
            .execute(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("delete progetto: {}", e)))?;

        pool.close().await;

        Ok(json!({
            "ok": true,
            "project_id": project_id_str,
            "name": project_name,
            "rows_affected": result.rows_affected(),
            "files_deleted": false,
            "message": "Progetto rimosso dal DB. I file su disco sono ancora presenti.",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["project_id", "confirm"],
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "UUID del progetto da eliminare"
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Deve essere true per procedere. Operazione distruttiva (DB only, file non toccati)."
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
    fn test_safety_no_fs() {
        let s = ProjectDeleteTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_write_filesystem);
        assert!(!s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_requires_confirm() {
        let s = ProjectDeleteTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "confirm"));
        assert!(required.iter().any(|v| v == "project_id"));
    }
}
