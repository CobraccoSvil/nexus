//! `github::gh_run_view` — `gh run view <id> --json`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhRunViewTool;

#[async_trait]
impl NexusToolHandler for GhRunViewTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let id = args
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("id required".into()))?
            .to_string();
        let out = run_cmd(
            "gh",
            &[
                "run",
                "view",
                &id,
                "--json",
                "name,status,conclusion,createdAt,updatedAt,event,headBranch,headSha,jobs",
            ],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(json!({}));
        Ok(json!({"ok": true, "run": parsed}))
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
