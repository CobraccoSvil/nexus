//! `vcs::git_clean_dry` — `git clean -nd` (dry-run, no actual removal).
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitCleanDryTool;

#[async_trait]
impl NexusToolHandler for GitCleanDryTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("git", &["clean", "-nd"], &ctx.project_root, ctx.timeout_secs).await?;
        let items: Vec<String> = out.stdout.lines()
            .filter_map(|l| l.strip_prefix("Would remove ").map(String::from))
            .collect();
        Ok(json!({
            "ok": out.success(),
            "would_remove_count": items.len(),
            "would_remove": items,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
