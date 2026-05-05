//! `vcs::git_check_ignore` — `git check-ignore -v <paths>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitCheckIgnoreTool;

#[async_trait]
impl NexusToolHandler for GitCheckIgnoreTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let paths: Vec<String> = args.get("paths").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if paths.is_empty() {
            return Err(NexusToolError::BadInput("paths required".into()));
        }
        let mut cmd: Vec<&str> = vec!["check-ignore", "-v"];
        for p in &paths { cmd.push(p); }
        let out = run_cmd("git", &cmd, &ctx.project_root, ctx.timeout_secs).await?;
        // exit 0 = ignored, exit 1 = not ignored — both ok
        let ignored: Vec<Value> = out.stdout.lines().map(|l| {
            let parts: Vec<&str> = l.splitn(3, ':').collect();
            json!({
                "source": parts.first().copied().unwrap_or(""),
                "line": parts.get(1).copied().unwrap_or(""),
                "pattern_path": parts.get(2).copied().unwrap_or("").trim(),
            })
        }).collect();
        Ok(json!({"ok": true, "ignored_count": ignored.len(), "ignored": ignored}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["paths"],"properties":{"paths":{"type":"array","items":{"type":"string"}}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
