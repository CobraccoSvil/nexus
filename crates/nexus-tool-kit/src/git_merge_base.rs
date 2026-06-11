//! `vcs::git_merge_base` — `git merge-base <a> <b>` common ancestor.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitMergeBaseTool;

#[async_trait]
impl NexusToolHandler for GitMergeBaseTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let a = args
            .get("a")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("a required".into()))?;
        let b = args
            .get("b")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("b required".into()))?;
        let out = run_cmd(
            "git",
            &["merge-base", a, b],
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
        Ok(json!({"ok": true, "a": a, "b": b, "merge_base": out.stdout.trim()}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["a","b"],"properties":{"a":{"type":"string"},"b":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
