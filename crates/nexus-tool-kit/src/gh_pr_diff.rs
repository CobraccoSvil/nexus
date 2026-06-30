//! `github::gh_pr_diff` — `gh pr diff <num>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrDiffTool;

#[async_trait]
impl NexusToolHandler for GhPrDiffTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let num = args
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?
            .to_string();
        let out = run_cmd(
            "gh",
            &["pr", "diff", &num],
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
        let added = out
            .stdout
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .count();
        let removed = out
            .stdout
            .lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .count();
        Ok(json!({
            "ok": true,
            "lines_added": added,
            "lines_removed": removed,
            "diff_preview": out.stdout.chars().take(8000).collect::<String>(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["number"],"properties":{"number":{"type":"integer"}}})
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
