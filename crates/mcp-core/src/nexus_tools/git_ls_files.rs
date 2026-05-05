//! `vcs::git_ls_files` — `git ls-files` lista file tracciati.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitLsFilesTool;

#[async_trait]
impl NexusToolHandler for GitLsFilesTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let max = args.get("max").and_then(Value::as_u64).unwrap_or(2000) as usize;
        let out = run_cmd("git", &["ls-files"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let files: Vec<&str> = out.stdout.lines().take(max).collect();
        let total = out.stdout.lines().count();
        Ok(json!({"ok": true, "total": total, "returned": files.len(), "files": files}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"max":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
