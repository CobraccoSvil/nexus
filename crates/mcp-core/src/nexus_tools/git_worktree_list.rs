//! `vcs::git_worktree_list` — `git worktree list --porcelain`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Map, Value};

pub struct GitWorktreeListTool;

#[async_trait]
impl NexusToolHandler for GitWorktreeListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("git", &["worktree", "list", "--porcelain"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let mut worktrees: Vec<Value> = Vec::new();
        let mut current = Map::new();
        for line in out.stdout.lines() {
            if line.is_empty() {
                if !current.is_empty() {
                    worktrees.push(Value::Object(std::mem::take(&mut current)));
                }
                continue;
            }
            if let Some((k, v)) = line.split_once(' ') {
                current.insert(k.to_string(), json!(v));
            } else {
                current.insert(line.to_string(), json!(true));
            }
        }
        if !current.is_empty() {
            worktrees.push(Value::Object(current));
        }
        Ok(json!({"ok": true, "count": worktrees.len(), "worktrees": worktrees}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
