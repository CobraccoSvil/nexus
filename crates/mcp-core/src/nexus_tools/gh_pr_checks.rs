//! `github::gh_pr_checks` — `gh pr checks <num>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrChecksTool;

#[async_trait]
impl NexusToolHandler for GhPrChecksTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let num = args.get("number").and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?
            .to_string();
        let out = run_cmd("gh", &["pr", "checks", &num], &ctx.project_root, ctx.timeout_secs).await?;
        let total = out.stdout.lines().count();
        let pass = out.stdout.lines().filter(|l| l.contains("pass")).count();
        let fail = out.stdout.lines().filter(|l| l.contains("fail")).count();
        let pending = out.stdout.lines().filter(|l| l.contains("pending")).count();
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "total": total,
            "pass": pass,
            "fail": fail,
            "pending": pending,
            "stdout_preview": out.stdout.chars().take(2000).collect::<String>(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["number"],"properties":{"number":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: true, network_egress: true }
    }
}
