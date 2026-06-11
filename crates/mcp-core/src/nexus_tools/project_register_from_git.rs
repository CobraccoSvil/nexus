//! `project_register_from_git` — clona un repository Git e lo registra come progetto.
//!
//! Esegue `git clone --depth=1 <url>` nella directory base dei progetti,
//! poi registra il progetto nel DB (projects, workspaces, repositories,
//! project_members, git_remotes). Transazione atomica.

use super::db_helper;
use super::project_register_common::{register_project_records, NewProjectRecord};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectRegisterFromGitTool;

#[async_trait]
impl NexusToolHandler for ProjectRegisterFromGitTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
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
        let default_branch = branch.unwrap_or_else(|| "main".to_string());
        let abs_path = target_dir.to_string_lossy().to_string();

        let (project_id, slug) = register_project_records(
            &pool,
            &NewProjectRecord {
                user_id: ctx.user_id,
                name: &project_name,
                default_branch: &default_branch,
                abs_path: &abs_path,
                remote_url: Some(&url),
                is_git_repo: true,
            },
        )
        .await?;

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
        assert_eq!(dir_name_from_url("git@github.com:user/repo.git"), "repo");
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
