//! `vcs::git_submodule_list` — `git submodule status` lista submodule.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitSubmoduleListTool;

#[async_trait]
impl NexusToolHandler for GitSubmoduleListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "git",
            &["submodule", "status"],
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
        let items: Vec<Value> = out
            .stdout
            .lines()
            .map(|raw| {
                let line = raw;
                // First char: ' '=ok, '-'=not initialized, '+'=different SHA, 'U'=conflict
                let status_char = line.chars().next().unwrap_or(' ');
                let rest = &line[1..];
                let parts: Vec<&str> = rest.splitn(3, ' ').collect();
                json!({
                    "status": status_char.to_string(),
                    "sha": parts.first().copied().unwrap_or(""),
                    "path": parts.get(1).copied().unwrap_or(""),
                    "describe": parts.get(2).copied().unwrap_or(""),
                })
            })
            .collect();
        Ok(json!({"ok": true, "count": items.len(), "submodules": items}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
