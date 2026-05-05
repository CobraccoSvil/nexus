//! `github::gh_run_cancel` — `gh run cancel <id>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhRunCancelTool;

#[async_trait]
impl NexusToolHandler for GhRunCancelTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let id = args.get("id").and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("id required".into()))?
            .to_string();
        let out = run_cmd("gh", &["run", "cancel", &id], &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({"ok": out.success(), "exit_code": out.exit_code, "stdout": out.stdout.trim()}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["id"],"properties":{"id":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: false, can_write_filesystem: false, can_execute_subproc: true, network_egress: true }
    }
}
