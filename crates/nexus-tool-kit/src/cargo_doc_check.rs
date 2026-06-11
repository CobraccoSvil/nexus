//! `documentation::cargo_doc_check` — `cargo doc --no-deps` con `RUSTDOCFLAGS=-D warnings`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoDocCheckTool;

#[async_trait]
impl NexusToolHandler for CargoDocCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        // RUSTDOCFLAGS=-D warnings deve essere settato dal chiamante; qui passiamo solo i flag.
        let out = run_cmd(
            "cargo",
            &["doc", "--no-deps", "--quiet"],
            &ctx.project_root,
            ctx.timeout_secs.max(300),
        )
        .await?;
        let warnings = out.stderr.matches("warning:").count();
        let errors = out.stderr.matches("error:").count();
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "doc_warnings": warnings,
            "doc_errors": errors,
            "stderr_preview": out.stderr.chars().take(2000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
