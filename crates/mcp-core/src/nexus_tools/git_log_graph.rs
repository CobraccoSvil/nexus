//! `vcs::git_log_graph` — `git log --oneline --graph -n N`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitLogGraphTool;

#[async_trait]
impl NexusToolHandler for GitLogGraphTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let n = args.get("n").and_then(Value::as_u64).unwrap_or(30).min(500).to_string();
        let out = run_cmd(
            "git",
            &["log", "--oneline", "--graph", "--decorate", "-n", &n],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let lines: Vec<&str> = out.stdout.lines().collect();
        Ok(json!({"ok": true, "count": lines.len(), "lines": lines}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"n":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
