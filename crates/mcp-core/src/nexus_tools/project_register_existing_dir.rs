//! `project_register_existing_dir` — registra una directory gia' presente come progetto.
//!
//! Non esegue clone: la directory deve gia' esistere sul filesystem.
//! Verifica esistenza, rileva info Git, registra nel DB con transazione.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

pub struct ProjectRegisterExistingDirTool;

#[async_trait]
impl NexusToolHandler for ProjectRegisterExistingDirTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let path_str = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'path' obbligatorio".into()))?
            .trim()
            .to_string();

        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string());

        let dir = std::path::PathBuf::from(&path_str);
        if !dir.is_dir() {
            return Err(NexusToolError::BadInput(format!(
                "Directory '{}' non esiste",
                path_str
            )));
        }

        let abs_path = dir
            .canonicalize()
            .map_err(|e| NexusToolError::BadInput(format!("canonicalize: {}", e)))?;

        let pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        // Verifica che non sia gia' registrato
        let existing: Option<Uuid> = sqlx::query_scalar(
            r#"SELECT p.id FROM projects p
               INNER JOIN workspaces w ON w.project_id = p.id
               WHERE w.absolute_path = $1 LIMIT 1"#,
        )
        .bind(abs_path.to_string_lossy().to_string())
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("lookup esistente: {}", e)))?;

        if let Some(existing_id) = existing {
            pool.close().await;
            return Ok(json!({
                "ok": true,
                "already_registered": true,
                "project_id": existing_id.to_string(),
                "message": "Progetto gia' registrato con questo percorso",
            }));
        }

        // Rileva info Git
        let git_output = tokio::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel", "--abbrev-ref", "HEAD"])
            .current_dir(&abs_path)
            .output()
            .await;

        let (is_git, current_branch) = match git_output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let lines: Vec<&str> = stdout.trim().lines().collect();
                let branch = lines.get(1).map(|s| s.to_string()).unwrap_or_else(|| "main".to_string());
                (true, branch)
            }
            _ => (false, "main".to_string()),
        };

        let project_name = name.unwrap_or_else(|| {
            abs_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Project".to_string())
        });

        let project_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let repository_id = Uuid::new_v4();

        let team_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM teams WHERE owner_user_id = $1 LIMIT 1",
        )
        .bind(ctx.user_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("lookup team: {}", e)))?
        .unwrap_or_else(Uuid::new_v4);

        let slug = format!(
            "{}-{}",
            project_name.to_lowercase().replace(' ', "-"),
            &project_id.to_string()[..8]
        );

        let abs_str = abs_path.to_string_lossy().to_string();

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("begin tx: {}", e)))?;

        sqlx::query(
            r#"INSERT INTO projects (id, team_id, owner_user_id, name, slug, default_branch, visibility, last_opened_by_user_id)
               VALUES ($1, $2, $3, $4, $5, $6, 'private', $3)"#,
        )
        .bind(project_id)
        .bind(team_id)
        .bind(ctx.user_id)
        .bind(&project_name)
        .bind(&slug)
        .bind(&current_branch)
        .execute(&mut *tx)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert projects: {}", e)))?;

        sqlx::query(
            "INSERT INTO project_members (id, project_id, user_id, role) VALUES ($1, $2, $3, 'owner')",
        )
        .bind(Uuid::new_v4())
        .bind(project_id)
        .bind(ctx.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert members: {}", e)))?;

        sqlx::query(
            "INSERT INTO workspaces (id, project_id, absolute_path, is_primary) VALUES ($1, $2, $3, TRUE)",
        )
        .bind(workspace_id)
        .bind(project_id)
        .bind(&abs_str)
        .execute(&mut *tx)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert workspaces: {}", e)))?;

        sqlx::query(
            r#"INSERT INTO repositories (id, project_id, provider, root_path, is_git_repo, current_branch)
               VALUES ($1, $2, 'local', $3, $4, $5)"#,
        )
        .bind(repository_id)
        .bind(project_id)
        .bind(&abs_str)
        .bind(is_git)
        .bind(&current_branch)
        .execute(&mut *tx)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert repositories: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("commit: {}", e)))?;

        pool.close().await;

        Ok(json!({
            "ok": true,
            "project_id": project_id.to_string(),
            "name": project_name,
            "slug": slug,
            "path": abs_str,
            "is_git_repo": is_git,
            "branch": current_branch,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Percorso assoluto della directory da registrare come progetto"
                },
                "name": {
                    "type": "string",
                    "description": "Nome del progetto. Se omesso, usa il nome della directory."
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_no_fs_write() {
        let s = ProjectRegisterExistingDirTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_requires_path() {
        let s = ProjectRegisterExistingDirTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "path"));
    }
}
