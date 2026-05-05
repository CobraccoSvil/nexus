//! `vcs::git_show_branch` — `git show-branch --all`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitShowBranchTool;

#[async_trait]
impl NexusToolHandler for GitShowBranchTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("git", &["show-branch", "--all"], &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({
            "ok": out.success(),
            "stdout_preview": out.stdout.chars().take(4000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
