//! `project_info` — informazioni generali del progetto: nome, root, git, stack, istruzioni custom, sandbox.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;
use crate::nexus_tools::db_helper::get_pool;

pub struct ProjectInfoTool;

#[async_trait]
impl NexusToolHandler for ProjectInfoTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pool = get_pool().await
            .map_err(|e| NexusToolError::BadInput(format!("db connect: {}", e)))?;

        let row = sqlx::query(
            r#"SELECT p.name, p.slug, p.default_branch,
                      p.custom_instructions,
                      p.sandbox_config,
                      p.analysis_json,
                      p.analyzed_at,
                      r.is_git_repo, r.current_branch, r.remote_url
               FROM projects p
               LEFT JOIN repositories r ON r.project_id = p.id
               WHERE p.id = $1"#
        )
        .bind(ctx.project_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("query: {}", e)))?;

        pool.close().await;

        let Some(row) = row else {
            return Ok(json!({
                "ok": false,
                "message": "Progetto non trovato."
            }));
        };

        let name: String = row.try_get("name").unwrap_or_default();
        let slug: String = row.try_get("slug").unwrap_or_default();
        let default_branch: Option<String> = row.try_get("default_branch").unwrap_or(None);
        let custom_instructions: Option<String> = row.try_get("custom_instructions").unwrap_or(None);
        let sandbox_config: Option<Value> = row.try_get("sandbox_config").unwrap_or(None);
        let analysis_json: Option<Value> = row.try_get("analysis_json").unwrap_or(None);
        let is_git_repo: Option<bool> = row.try_get("is_git_repo").unwrap_or(None);
        let current_branch: Option<String> = row.try_get("current_branch").unwrap_or(None);
        let remote_url: Option<String> = row.try_get("remote_url").unwrap_or(None);

        // Estrai summary dallo analysis_json se presente
        let stack_summary = analysis_json.as_ref().and_then(|a| {
            a.get("summary").and_then(|s| s.as_str()).map(String::from)
        });

        Ok(json!({
            "ok": true,
            "name": name,
            "slug": slug,
            "project_root": ctx.project_root.to_string_lossy(),
            "git": {
                "is_git_repo": is_git_repo.unwrap_or(false),
                "current_branch": current_branch,
                "default_branch": default_branch,
                "remote_url": remote_url,
            },
            "custom_instructions": custom_instructions,
            "sandbox_config": sandbox_config,
            "stack_summary": stack_summary,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: false, network_egress: true }
    }
}
