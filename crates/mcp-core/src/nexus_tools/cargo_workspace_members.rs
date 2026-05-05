//! `build::cargo_workspace_members` — `cargo metadata` -> workspace members.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoWorkspaceMembersTool;

#[async_trait]
impl NexusToolHandler for CargoWorkspaceMembersTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("cargo", &["metadata", "--format-version=1", "--no-deps"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| json!({}));
        let members: Vec<String> = parsed.get("workspace_members")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(json!({"ok": true, "count": members.len(), "members": members, "duration_ms": out.duration_ms}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
