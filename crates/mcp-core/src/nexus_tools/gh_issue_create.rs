//! `github::gh_issue_create` — `gh issue create --title --body`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhIssueCreateTool;

#[async_trait]
impl NexusToolHandler for GhIssueCreateTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("title required".into()))?;
        let body = args.get("body").and_then(Value::as_str).unwrap_or("");
        let out = run_cmd(
            "gh",
            &["issue", "create", "--title", title, "--body", body],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "stdout": out.stdout.trim(),
            "stderr_preview": out.stderr.chars().take(1000).collect::<String>(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["title"],"properties":{"title":{"type":"string"},"body":{"type":"string"}}})
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
