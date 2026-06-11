//! `github::gh_run_list` — `gh run list --json ...` (workflow runs).

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhRunListTool;

#[async_trait]
impl NexusToolHandler for GhRunListTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .min(100)
            .to_string();

        let mut cmd_args: Vec<&str> = vec!["run", "list", "--limit", &limit, "--json"];
        cmd_args.push("databaseId,name,displayTitle,status,conclusion,workflowName,headBranch,event,startedAt,createdAt");

        let workflow_arg;
        if let Some(w) = args.get("workflow").and_then(Value::as_str) {
            cmd_args.push("--workflow");
            workflow_arg = w.to_string();
            cmd_args.push(&workflow_arg);
        }

        let out = run_cmd("gh", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| json!([]));
        let runs = parsed.as_array().cloned().unwrap_or_default();
        let failed = runs
            .iter()
            .filter(|r| r.get("conclusion").and_then(Value::as_str) == Some("failure"))
            .count();
        let success = runs
            .iter()
            .filter(|r| r.get("conclusion").and_then(Value::as_str) == Some("success"))
            .count();

        Ok(json!({
            "ok": true,
            "count": runs.len(),
            "success": success,
            "failed": failed,
            "runs": runs,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "workflow": {"type": "string", "description": "filename or workflow name"}
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
        assert!(GhRunListTool.safety().network_egress);
    }
}
