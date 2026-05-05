//! `build::build_workspace_check` — `cargo check --workspace --quiet`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildWorkspaceCheckTool;

#[async_trait]
impl NexusToolHandler for BuildWorkspaceCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("cargo", &["check", "--workspace", "--quiet"], &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "compile_errors": out.stderr.lines().filter(|l| l.contains("error[") || l.contains("error:")).count(),
            "warnings": out.stderr.lines().filter(|l| l.contains("warning")).count(),
            "stderr_tail": out.stderr.lines().rev().take(10).collect::<Vec<_>>(),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::write_subproc() }
}
