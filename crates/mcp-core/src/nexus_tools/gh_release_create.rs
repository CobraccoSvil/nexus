//! `github::gh_release_create` — `gh release create <tag>`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhReleaseCreateTool;

#[async_trait]
impl NexusToolHandler for GhReleaseCreateTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let tag = args
            .get("tag")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("tag required".into()))?;
        let title = args.get("title").and_then(Value::as_str).unwrap_or(tag);
        let notes = args.get("notes").and_then(Value::as_str).unwrap_or("");
        let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);
        let mut cmd: Vec<&str> = vec!["release", "create", tag, "--title", title, "--notes", notes];
        if draft {
            cmd.push("--draft");
        }
        let out = run_cmd("gh", &cmd, &ctx.project_root, ctx.timeout_secs).await?;
        Ok(
            json!({"ok": out.success(), "exit_code": out.exit_code, "tag": tag, "stdout": out.stdout.trim()}),
        )
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["tag"],"properties":{"tag":{"type":"string"},"title":{"type":"string"},"notes":{"type":"string"},"draft":{"type":"boolean"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}
