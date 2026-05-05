//! `github::gh_workflow_view` — `gh workflow view <name>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhWorkflowViewTool;

#[async_trait]
impl NexusToolHandler for GhWorkflowViewTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let name = args.get("name").and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("name required".into()))?;
        let out = run_cmd("gh", &["workflow", "view", name], &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "stdout_preview": out.stdout.chars().take(4000).collect::<String>(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["name"],"properties":{"name":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: true, network_egress: true }
    }
}
