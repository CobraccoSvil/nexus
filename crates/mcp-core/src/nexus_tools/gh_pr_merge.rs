//! `github::gh_pr_merge` — `gh pr merge <num> --squash|--merge|--rebase`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrMergeTool;

#[async_trait]
impl NexusToolHandler for GhPrMergeTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let num = args
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?
            .to_string();
        let strategy = args
            .get("strategy")
            .and_then(Value::as_str)
            .unwrap_or("squash");
        let strat_flag = match strategy {
            "merge" => "--merge",
            "rebase" => "--rebase",
            _ => "--squash",
        };
        let out = run_cmd(
            "gh",
            &["pr", "merge", &num, strat_flag, "--auto"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "strategy": strategy,
            "stdout": out.stdout.trim(),
            "stderr_preview": out.stderr.chars().take(800).collect::<String>(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["number"],"properties":{"number":{"type":"integer"},"strategy":{"type":"string","enum":["squash","merge","rebase"]}}})
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
