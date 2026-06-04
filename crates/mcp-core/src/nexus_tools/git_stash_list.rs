//! `vcs::git_stash_list` — `git stash list --pretty=...`.
//!
//! Output: `{count, items: [{index, ref, branch, message}]}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitStashListTool;

#[async_trait]
impl NexusToolHandler for GitStashListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "git",
            &["stash", "list", "--pretty=format:%gd%x09%gs"],
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

        let mut items = Vec::new();
        for line in out.stdout.lines() {
            let mut cols = line.splitn(2, '\t');
            let stash_ref = cols.next().unwrap_or("").trim().to_string();
            let message = cols.next().unwrap_or("").trim().to_string();
            // git stash refs look like: stash@{0}
            let index = stash_ref
                .trim_start_matches("stash@{")
                .trim_end_matches('}')
                .parse::<u32>()
                .ok();
            // message format usually: "WIP on branch: <hash> <subject>" or "On branch: <subject>"
            let branch = message
                .strip_prefix("WIP on ")
                .or_else(|| message.strip_prefix("On "))
                .and_then(|s| s.split(':').next())
                .map(|s| s.to_string());
            items.push(json!({
                "index": index,
                "ref": stash_ref,
                "branch": branch,
                "message": message,
            }));
        }

        Ok(json!({
            "ok": true,
            "count": items.len(),
            "items": items,
            "duration_ms": out.duration_ms,
        }))
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_safety() {
        assert!(GitStashListTool.safety().read_only);
        assert!(GitStashListTool.safety().can_execute_subproc);
    }
}
