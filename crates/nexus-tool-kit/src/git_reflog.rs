//! `vcs::git_reflog` — `git reflog -n N` reference log.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitReflogTool;

#[async_trait]
impl NexusToolHandler for GitReflogTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let n = args
            .get("n")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .min(500)
            .to_string();
        let out = run_cmd(
            "git",
            &["reflog", "-n", &n],
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
        let entries: Vec<Value> = out
            .stdout
            .lines()
            .map(|l| {
                let parts: Vec<&str> = l.splitn(3, ' ').collect();
                json!({
                    "sha": parts.first().copied().unwrap_or(""),
                    "ref": parts.get(1).copied().unwrap_or(""),
                    "message": parts.get(2).copied().unwrap_or(""),
                })
            })
            .collect();
        Ok(json!({"ok": true, "count": entries.len(), "entries": entries}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"n":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
