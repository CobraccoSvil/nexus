//! `github::gh_pr_review` — `gh pr review <num> --approve|--request-changes|--comment`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrReviewTool;

#[async_trait]
impl NexusToolHandler for GhPrReviewTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let num = args
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?
            .to_string();
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("comment");
        let body = args.get("body").and_then(Value::as_str).unwrap_or("");
        let action_flag = match action {
            "approve" => "--approve",
            "request-changes" => "--request-changes",
            _ => "--comment",
        };
        let mut cmd: Vec<&str> = vec!["pr", "review", &num, action_flag];
        if !body.is_empty() {
            cmd.push("--body");
            cmd.push(body);
        }
        let out = run_cmd("gh", &cmd, &ctx.project_root, ctx.timeout_secs).await?;
        Ok(
            json!({"ok": out.success(), "exit_code": out.exit_code, "action": action, "stdout": out.stdout.trim()}),
        )
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["number"],"properties":{"number":{"type":"integer"},"action":{"type":"string"},"body":{"type":"string"}}})
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
