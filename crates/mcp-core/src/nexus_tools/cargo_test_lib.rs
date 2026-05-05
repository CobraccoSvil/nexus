//! `testing::cargo_test_lib` — `cargo test --lib`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoTestLibTool;

#[async_trait]
impl NexusToolHandler for CargoTestLibTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("cargo", &["test", "--lib"], &ctx.project_root, ctx.timeout_secs.max(300)).await?;
        let passed = out.stdout.lines().filter(|l| l.contains(" ... ok")).count();
        let failed = out.stdout.lines().filter(|l| l.contains(" ... FAILED")).count();
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "passed": passed,
            "failed": failed,
            "stdout_preview": out.stdout.chars().take(2000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::write_subproc() }
}
