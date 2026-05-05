//! `vcs::git_describe` — `git describe --tags --always --long --dirty`.
//!
//! Output: `{description, tag, commits_since_tag, sha, dirty}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitDescribeTool;

#[async_trait]
impl NexusToolHandler for GitDescribeTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "git",
            &["describe", "--tags", "--always", "--long", "--dirty"],
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

        let description = out.stdout.trim().to_string();
        let dirty = description.ends_with("-dirty");
        let body = description.trim_end_matches("-dirty");

        // Format: <tag>-<commits>-g<sha>  oppure soltanto <sha> se nessun tag
        let mut tag: Option<String> = None;
        let mut commits_since: Option<u32> = None;

        let parts: Vec<&str> = body.rsplitn(3, '-').collect();
        let sha: Option<String> = if parts.len() == 3 {
            // parts is reversed: [g<sha>, <commits>, <tag>]
            commits_since = parts[1].parse::<u32>().ok();
            tag = Some(parts[2].to_string());
            Some(parts[0].trim_start_matches('g').to_string())
        } else {
            Some(body.to_string())
        };

        Ok(json!({
            "ok": true,
            "description": description,
            "tag": tag,
            "commits_since_tag": commits_since,
            "sha": sha,
            "dirty": dirty,
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
        assert!(GitDescribeTool.safety().read_only);
    }
}
