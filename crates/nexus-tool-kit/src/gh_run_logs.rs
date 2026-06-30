//! `github::gh_run_logs` — `gh run view <id> --log` recente run logs.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhRunLogsTool;

#[async_trait]
impl NexusToolHandler for GhRunLogsTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("id required".into()))?
            .to_string();
        let out = run_cmd(
            "gh",
            &["run", "view", &id, "--log"],
            &ctx.project_root,
            ctx.timeout_secs.max(180),
        )
        .await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "log_size": out.stdout.len(),
            "log_preview": out.stdout.chars().take(8000).collect::<String>(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["id"],"properties":{"id":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}
