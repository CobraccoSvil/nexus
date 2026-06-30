//! `github::gh_pr_close` — `gh pr close <num>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrCloseTool;

#[async_trait]
impl NexusToolHandler for GhPrCloseTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let num = args
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?
            .to_string();
        let out = run_cmd(
            "gh",
            &["pr", "close", &num],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        Ok(json!({"ok": out.success(), "exit_code": out.exit_code, "stdout": out.stdout.trim()}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["number"],"properties":{"number":{"type":"integer"}}})
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
