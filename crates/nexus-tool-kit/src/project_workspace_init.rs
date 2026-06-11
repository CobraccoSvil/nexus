//! `project_workspace_init` — inizializza la riga `workspaces` per un progetto.
//!
//! Dopo un clone manuale o una registrazione incompleta, la riga in
//! `workspaces` puo' mancare. Questo tool verifica e crea la riga
//! se necessario, rendendo il progetto navigabile dall'agente.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

pub struct ProjectWorkspaceInitTool;

#[async_trait]
impl NexusToolHandler for ProjectWorkspaceInitTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path_str = args
            .get("path")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string());

        // Se non specificato, usa project_root dal contesto
        let abs_path = match path_str {
            Some(p) if !p.is_empty() => {
                let dir = std::path::PathBuf::from(&p);
                if !dir.is_dir() {
                    return Err(NexusToolError::BadInput(format!(
                        "Directory '{}' non esiste",
                        p
                    )));
                }
                dir.canonicalize()
                    .map_err(|e| NexusToolError::BadInput(format!("canonicalize: {}", e)))?
                    .to_string_lossy()
                    .to_string()
            }
            _ => ctx.project_root.to_string_lossy().to_string(),
        };

        let is_primary = args
            .get("is_primary")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        // Verifica che il progetto esista
        let project_exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM projects WHERE id = $1")
                .bind(ctx.project_id)
                .fetch_optional(&pool)
                .await
                .map_err(|e| NexusToolError::BadInput(format!("lookup progetto: {}", e)))?;

        if project_exists.is_none() {
            pool.close().await;
            return Err(NexusToolError::BadInput(format!(
                "Progetto '{}' non trovato",
                ctx.project_id
            )));
        }

        // Verifica se esiste gia' un workspace con questo path
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM workspaces WHERE project_id = $1 AND absolute_path = $2 LIMIT 1",
        )
        .bind(ctx.project_id)
        .bind(&abs_path)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("lookup workspace: {}", e)))?;

        if let Some(ws_id) = existing {
            pool.close().await;
            return Ok(json!({
                "ok": true,
                "already_exists": true,
                "workspace_id": ws_id.to_string(),
                "path": abs_path,
                "message": "Workspace gia' presente per questo progetto e percorso",
            }));
        }

        // Se is_primary, disattiva eventuali altri primary
        if is_primary {
            sqlx::query(
                "UPDATE workspaces SET is_primary = FALSE WHERE project_id = $1 AND is_primary = TRUE",
            )
            .bind(ctx.project_id)
            .execute(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("reset primary: {}", e)))?;
        }

        let workspace_id = Uuid::new_v4();

        sqlx::query(
            "INSERT INTO workspaces (id, project_id, absolute_path, is_primary) VALUES ($1, $2, $3, $4)",
        )
        .bind(workspace_id)
        .bind(ctx.project_id)
        .bind(&abs_path)
        .bind(is_primary)
        .execute(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert workspace: {}", e)))?;

        pool.close().await;

        Ok(json!({
            "ok": true,
            "workspace_id": workspace_id.to_string(),
            "project_id": ctx.project_id.to_string(),
            "path": abs_path,
            "is_primary": is_primary,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Percorso assoluto del workspace. Se omesso, usa la project_root corrente."
                },
                "is_primary": {
                    "type": "boolean",
                    "description": "Se true (default), questo workspace diventa il primario del progetto."
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
        let s = ProjectWorkspaceInitTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_write_filesystem);
        assert!(!s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_optional_path() {
        let s = ProjectWorkspaceInitTool.input_schema();
        // path non e' obbligatorio
        let required = s.get("required");
        assert!(required.is_none());
    }
}
