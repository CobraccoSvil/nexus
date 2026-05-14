//! `project_register_from_git` — clona un repository Git e lo registra come progetto.
//!
//! Esegue `git clone --depth=1 <url>` nella directory base dei progetti,
//! poi registra il progetto nel DB (projects, workspaces, repositories,
//! project_members, git_remotes). Transazione atomica.

use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub struct ProjectRegisterFromGitTool;

#[async_trait]
impl NexusToolHandler for ProjectRegisterFromGitTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'url' obbligatorio".into()))?
            .trim()
            .to_string();

        if url.is_empty() {
            return Err(NexusToolError::BadInput("URL vuoto".into()));
        }

        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string());

        let branch = args
            .get("branch")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string());

        let pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        // Leggi la base root dei progetti
        let base_root: String = sqlx::query_scalar(
            "SELECT value FROM settings WHERE key = 'projects_base_root' LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("query settings: {}", e)))?
        .unwrap_or_else(|| "/home/administrator/projects".to_string());

        // Ricava nome directory dall'URL
        let dir_name = name.clone().unwrap_or_else(|| dir_name_from_url(&url));
        let target_dir = std::path::PathBuf::from(&base_root).join(&dir_name);

        if target_dir.exists() {
            pool.close().await;
            return Err(NexusToolError::BadInput(format!(
                "Directory '{}' esiste gia'. Usa un nome diverso o project_register_existing_dir.",
                target_dir.display()
            )));
        }

        // Esegui git clone
        let mut clone_args = vec!["clone", "--depth=1"];
        if let Some(ref b) = branch {
            clone_args.push("-b");
            clone_args.push(b);
        }
        clone_args.push(&url);
        clone_args.push(target_dir.to_str().unwrap_or(&dir_name));

        let clone_output = tokio::process::Command::new("git")
            .args(&clone_args)
            .output()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("git clone fallito: {}", e)))?;

        if !clone_output.status.success() {
            let stderr = String::from_utf8_lossy(&clone_output.stderr);
            pool.close().await;
            return Err(NexusToolError::BadInput(format!(
                "git clone fallito (exit {}): {}",
                clone_output.status.code().unwrap_or(-1),
                stderr.chars().take(500).collect::<String>()
            )));
        }

        // Registra nel DB con transazione
        let project_name = name.unwrap_or_else(|| dir_name.clone());
        let project_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let repository_id = Uuid::new_v4();

        // Trova team_id dell'utente
        let team_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM teams WHERE owner_user_id = $1 LIMIT 1",
        )
        .bind(ctx.user_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("lookup team: {}", e)))?
        .unwrap_or_else(Uuid::new_v4);

        let default_branch = branch.unwrap_or_else(|| "main".to_string());
        let abs_path = target_dir.to_string_lossy().to_string();

        // Slug unico
        let slug = format!(
            "{}-{}",
            project_name.to_lowercase().replace(' ', "-"),
            &project_id.to_string()[..8]
        );

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
        .bind(&default_branch)
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
        .map_err(|e| NexusToolError::BadInput(format!("insert project_members: {}", e)))?;

        sqlx::query(
            "INSERT INTO workspaces (id, project_id, absolute_path, is_primary) VALUES ($1, $2, $3, TRUE)",
        )
        .bind(workspace_id)
        .bind(project_id)
        .bind(&abs_path)
        .execute(&mut *tx)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert workspaces: {}", e)))?;

        sqlx::query(
            r#"INSERT INTO repositories (id, project_id, provider, remote_url, root_path, is_git_repo, current_branch)
               VALUES ($1, $2, 'local', $3, $4, TRUE, $5)"#,
        )
        .bind(repository_id)
        .bind(project_id)
        .bind(&url)
        .bind(&abs_path)
        .bind(&default_branch)
        .execute(&mut *tx)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("insert repositories: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("commit tx: {}", e)))?;

        pool.close().await;

        Ok(json!({
            "ok": true,
            "project_id": project_id.to_string(),
            "name": project_name,
            "slug": slug,
            "path": abs_path,
            "branch": default_branch,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL del repository Git da clonare (es. https://github.com/user/repo)"
                },
                "name": {
                    "type": "string",
                    "description": "Nome del progetto. Se omesso, ricavato dall'URL."
                },
                "branch": {
                    "type": "string",
                    "description": "Branch da clonare. Default: il branch predefinito del repo."
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: true,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

/// Ricava il nome directory dall'URL Git (ultimo segmento senza .git).
fn dir_name_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let last_segment = trimmed.rsplit('/').next().unwrap_or("project");
    last_segment
        .strip_suffix(".git")
        .unwrap_or(last_segment)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dir_name_from_url_https() {
        assert_eq!(
            dir_name_from_url("https://github.com/user/my-repo.git"),
            "my-repo"
        );
    }

    #[test]
    fn test_dir_name_from_url_no_git_suffix() {
        assert_eq!(
            dir_name_from_url("https://github.com/user/my-repo"),
            "my-repo"
        );
    }

    #[test]
    fn test_dir_name_from_url_ssh() {
        assert_eq!(
            dir_name_from_url("git@github.com:user/repo.git"),
            "repo"
        );
    }

    #[test]
    fn test_safety_full_access() {
        let s = ProjectRegisterFromGitTool.safety();
        assert!(!s.read_only);
        assert!(s.can_write_filesystem);
        assert!(s.can_execute_subproc);
        assert!(s.network_egress);
    }
}
