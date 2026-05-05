//! `github::gh_pr_files` — `gh pr view <num> --json files`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrFilesTool;

#[async_trait]
impl NexusToolHandler for GhPrFilesTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let num = args.get("number").and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("number required".into()))?
            .to_string();
        let out = run_cmd("gh", &["pr", "view", &num, "--json", "files"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(json!({}));
        let files = parsed.get("files").cloned().unwrap_or(json!([]));
        let count = files.as_array().map(|a| a.len()).unwrap_or(0);
        Ok(json!({"ok": true, "count": count, "files": files}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["number"],"properties":{"number":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: true, network_egress: true }
    }
}
