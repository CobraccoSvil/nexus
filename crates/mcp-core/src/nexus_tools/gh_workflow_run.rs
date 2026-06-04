//! `github::gh_workflow_run` — `gh workflow run <name>` triggers a workflow.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhWorkflowRunTool;

#[async_trait]
impl NexusToolHandler for GhWorkflowRunTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("name required".into()))?;
        let r#ref = args.get("ref").and_then(Value::as_str).unwrap_or("main");
        let out = run_cmd(
            "gh",
            &["workflow", "run", name, "--ref", r#ref],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "name": name,
            "ref": r#ref,
            "stdout": out.stdout.trim(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["name"],"properties":{"name":{"type":"string"},"ref":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}
