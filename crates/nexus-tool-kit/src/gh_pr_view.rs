//! `github::gh_pr_view` — `gh pr view <number> --json ...`.
//!
//! Input: `{number}`. Ritorna i dettagli del PR.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrViewTool;

#[async_trait]
impl NexusToolHandler for GhPrViewTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let number = args
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?;
        let number_str = number.to_string();

        let cmd_args: Vec<&str> = vec![
            "pr",
            "view",
            &number_str,
            "--json",
            "number,title,body,state,author,headRefName,baseRefName,createdAt,updatedAt,isDraft,mergeable,additions,deletions,changedFiles,reviewDecision,url",
        ];

        let out = run_cmd("gh", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| json!({}));

        Ok(json!({
            "ok": true,
            "pr": parsed,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["number"],
            "properties": {
                "number": {"type": "integer", "minimum": 1}
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
        assert!(GhPrViewTool.safety().network_egress);
    }
}
