//! `deployment::deploy_env_files_count` — count .env / .env.* / .envrc files.
use super::fs_scan::walk_project_files_keep_dotfiles;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeployEnvFilesCountTool;

#[async_trait]
impl NexusToolHandler for DeployEnvFilesCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let found = walk_project_files_keep_dotfiles(&ctx.project_root, 4, &|name| {
            name == ".env" || name.starts_with(".env.") || name == ".envrc"
        });
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
