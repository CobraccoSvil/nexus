//! `vcs::git_ls_tree` — `git ls-tree -r <ref>` lista file in un commit.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitLsTreeTool;

#[async_trait]
impl NexusToolHandler for GitLsTreeTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let r = args.get("ref").and_then(Value::as_str).unwrap_or("HEAD");
        let out = run_cmd("git", &["ls-tree", "-r", r], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let entries: Vec<Value> = out.stdout.lines().take(1000).map(|l| {
            // mode SP type SP sha TAB path
            let (head, path) = l.split_once('\t').unwrap_or((l, ""));
            let parts: Vec<&str> = head.split_whitespace().collect();
            json!({
                "mode": parts.first().copied().unwrap_or(""),
                "type": parts.get(1).copied().unwrap_or(""),
                "sha": parts.get(2).copied().unwrap_or(""),
                "path": path,
            })
        }).collect();
        Ok(json!({"ok": true, "ref": r, "count": entries.len(), "entries": entries}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"ref":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
