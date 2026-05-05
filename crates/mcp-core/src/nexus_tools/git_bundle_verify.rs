//! `vcs::git_bundle_verify` — `git bundle verify <path>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitBundleVerifyTool;

#[async_trait]
impl NexusToolHandler for GitBundleVerifyTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args.get("path").and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let p_in = std::path::Path::new(path);
        if p_in.is_absolute() || p_in.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let out = run_cmd("git", &["bundle", "verify", path], &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "stdout": out.stdout,
            "stderr": out.stderr,
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
