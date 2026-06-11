//! `github::gh_pr_list` — `gh pr list --json ...`.
//!
//! Input: `{state?, limit?, base?}`.
//! Output: `{count, items: [...]}` con JSON parsato.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrListTool;

#[async_trait]
impl NexusToolHandler for GhPrListTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let state = args.get("state").and_then(Value::as_str).unwrap_or("open");
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(30)
            .min(500);
        let limit_str = limit.to_string();

        let mut cmd_args: Vec<&str> = vec![
            "pr",
            "list",
            "--state",
            state,
            "--limit",
            &limit_str,
            "--json",
            "number,title,state,author,headRefName,baseRefName,createdAt,updatedAt,isDraft,url",
        ];
        let base_arg;
        if let Some(b) = args.get("base").and_then(Value::as_str) {
            base_arg = b.to_string();
            cmd_args.push("--base");
            cmd_args.push(&base_arg);
        }

        let out = run_cmd("gh", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| json!([]));
        let count = parsed.as_array().map(|a| a.len()).unwrap_or(0);

        Ok(json!({
            "ok": true,
            "state": state,
            "count": count,
            "items": parsed,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "state": {"type": "string", "enum": ["open", "closed", "merged", "all"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500},
                "base": {"type": "string"}
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
        let s = GhPrListTool.safety();
        assert!(s.network_egress && s.can_execute_subproc);
    }
}
