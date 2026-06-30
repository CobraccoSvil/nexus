//! `vcs::git_fsck` — `git fsck --no-progress` repo integrity check.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitFsckTool;

#[async_trait]
impl NexusToolHandler for GitFsckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "git",
            &["fsck", "--no-progress", "--no-dangling"],
            &ctx.project_root,
            ctx.timeout_secs.max(180),
        )
        .await?;
        let issues = out.stderr.lines().filter(|l| !l.is_empty()).count();
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "issue_lines": issues,
            "stderr_preview": out.stderr.chars().take(2000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
