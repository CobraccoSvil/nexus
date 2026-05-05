//! `github::gh_release_view` — `gh release view <tag> --json`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhReleaseViewTool;

#[async_trait]
impl NexusToolHandler for GhReleaseViewTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let tag = args.get("tag").and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("tag required".into()))?;
        let out = run_cmd(
            "gh",
            &["release", "view", tag, "--json", "tagName,name,publishedAt,isDraft,isPrerelease,assets,body"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(json!({}));
        Ok(json!({"ok": true, "release": parsed}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["tag"],"properties":{"tag":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety { read_only: true, can_write_filesystem: false, can_execute_subproc: true, network_egress: true }
    }
}
