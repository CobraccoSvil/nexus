//! `github::gh_workflow_list` — `gh workflow list --json ...`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhWorkflowListTool;

#[async_trait]
impl NexusToolHandler for GhWorkflowListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .min(200)
            .to_string();
        let out = run_cmd(
            "gh",
            &[
                "workflow",
                "list",
                "--limit",
                &limit,
                "--json",
                "id,name,state,path",
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

        let parsed: Value = serde_json::from_str(&out.stdout)
            .unwrap_or_else(|_| json!([]));
        let count = parsed.as_array().map(|a| a.len()).unwrap_or(0);

        Ok(json!({
            "ok": true,
            "count": count,
            "workflows": parsed,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 200}
            }
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_safety() {
        let s = GhWorkflowListTool.safety();
        assert!(s.network_egress);
        assert!(s.read_only);
    }
}
