//! `performance::perf_compile_units` — numero crate compilati via `cargo metadata`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfCompileUnitsTool;

#[async_trait]
impl NexusToolHandler for PerfCompileUnitsTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "cargo",
            &["metadata", "--format-version=1", "--no-deps"],
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
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
        let workspace = parsed
            .get("packages")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        let workspace_members = parsed
            .get("workspace_members")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        Ok(json!({
            "ok": true,
            "workspace_packages": workspace,
            "workspace_members": workspace_members,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
