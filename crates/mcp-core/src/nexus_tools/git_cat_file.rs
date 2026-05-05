//! `vcs::git_cat_file` — `git cat-file -p <ref>` show object content (preview).
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitCatFileTool;

#[async_trait]
impl NexusToolHandler for GitCatFileTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let r = args.get("ref").and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("ref required".into()))?;
        let out = run_cmd("git", &["cat-file", "-p", r], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let preview: String = out.stdout.chars().take(4000).collect();
        Ok(json!({"ok": true, "ref": r, "size": out.stdout.len(), "content_preview": preview}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["ref"],"properties":{"ref":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
