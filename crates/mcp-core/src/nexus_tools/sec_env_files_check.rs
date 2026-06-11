//! `security::sec_env_files_check` — find .env* files and check .gitignore coverage.
use super::fs_scan::walk_project_files_keep_dotfiles;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecEnvFilesCheckTool;

#[async_trait]
impl NexusToolHandler for SecEnvFilesCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let env_files =
            walk_project_files_keep_dotfiles(&ctx.project_root, 4, &|name| name.starts_with(".env"));
        let gi = ctx.project_root.join(".gitignore");
        let gi_content = std::fs::read_to_string(&gi).unwrap_or_default();
        let env_in_gitignore = gi_content.contains(".env");
        Ok(json!({
            "ok": true,
            "env_files": env_files,
            "count": env_files.len(),
            "gitignore_present": gi.is_file(),
            "env_in_gitignore": env_in_gitignore,
            "warning": !env_files.is_empty() && !env_in_gitignore,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
