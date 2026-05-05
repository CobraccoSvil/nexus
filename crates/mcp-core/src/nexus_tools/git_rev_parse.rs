//! `vcs::git_rev_parse` — `git rev-parse <ref>` per risolvere ref → SHA.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitRevParseTool;

#[async_trait]
impl NexusToolHandler for GitRevParseTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let r = args.get("ref").and_then(Value::as_str).unwrap_or("HEAD");
        let out = run_cmd("git", &["rev-parse", r], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        Ok(json!({"ok": true, "ref": r, "sha": out.stdout.trim()}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"ref":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
