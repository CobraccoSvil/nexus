//! `github::gh_repo_fork_list` — `gh repo view --json forkCount,parent`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhRepoForkListTool;

#[async_trait]
impl NexusToolHandler for GhRepoForkListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "gh",
            &["repo", "view", "--json", "forkCount,parent,isFork,nameWithOwner"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(json!({}));
        Ok(json!({"ok": true, "info": parsed}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: true, network_egress: true }
    }
}
