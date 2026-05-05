//! `github::gh_repo_view` — `gh repo view --json ...`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhRepoViewTool;

#[async_trait]
impl NexusToolHandler for GhRepoViewTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "gh",
            &[
                "repo",
                "view",
                "--json",
                "name,nameWithOwner,description,defaultBranchRef,isPrivate,isFork,isArchived,languages,licenseInfo,stargazerCount,forkCount,issues,pullRequests,createdAt,updatedAt,url,visibility",
            ],
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

        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| json!({}));

        Ok(json!({
            "ok": true,
            "repo": parsed,
            "duration_ms": out.duration_ms,
        }))
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_safety() {
        assert!(GhRepoViewTool.safety().network_egress);
    }
}
