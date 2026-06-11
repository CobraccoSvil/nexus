//! `github::gh_issue_comment` — `gh issue comment <num> --body`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhIssueCommentTool;

#[async_trait]
impl NexusToolHandler for GhIssueCommentTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let num = args
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?
            .to_string();
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("body required".into()))?;
        let out = run_cmd(
            "gh",
            &["issue", "comment", &num, "--body", body],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "stdout": out.stdout.trim(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["number","body"],"properties":{"number":{"type":"integer"},"body":{"type":"string"}}})
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
