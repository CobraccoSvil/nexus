//! `performance::perf_compile_units` — numero crate compilati via `cargo metadata`.
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
        let (parsed, _duration_ms) = super::run_cargo_metadata_json(ctx).await?;
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
