//! `code_analysis::cargo_check_release` — `cargo check --release`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoCheckReleaseTool;

#[async_trait]
impl NexusToolHandler for CargoCheckReleaseTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "cargo",
            &["check", "--release", "--message-format=short"],
            &ctx.project_root,
            ctx.timeout_secs.max(300),
        )
        .await?;
        let warnings = out.stderr.matches("warning:").count();
        let errors = out.stderr.matches("error:").count();
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "warnings": warnings,
            "errors": errors,
            "stderr_preview": out.stderr.chars().take(2000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
