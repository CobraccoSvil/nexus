//! `build::cargo_publish_dry` — `cargo publish --dry-run --allow-dirty`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoPublishDryTool;

#[async_trait]
impl NexusToolHandler for CargoPublishDryTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "cargo",
            &["publish", "--dry-run", "--allow-dirty"],
            &ctx.project_root,
            ctx.timeout_secs.max(180),
        )
        .await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "stderr_preview": out.stderr.chars().take(3000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::write_subproc() }
}
