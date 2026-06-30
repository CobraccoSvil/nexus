//! `vcs::git_for_each_ref` — itera tutte le ref del repo.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitForEachRefTool;

#[async_trait]
impl NexusToolHandler for GitForEachRefTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "git",
            &[
                "for-each-ref",
                "--format=%(refname)|%(objecttype)|%(objectname)",
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
        let refs: Vec<Value> = out
            .stdout
            .lines()
            .map(|l| {
                let parts: Vec<&str> = l.split('|').collect();
                json!({
                    "ref": parts.first().copied().unwrap_or(""),
                    "type": parts.get(1).copied().unwrap_or(""),
                    "sha": parts.get(2).copied().unwrap_or(""),
                })
            })
            .collect();
        Ok(json!({"ok": true, "count": refs.len(), "refs": refs}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
