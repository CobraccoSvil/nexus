//! `testing::test_run_integration` — `cargo test --tests --quiet`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestRunIntegrationTool;

#[async_trait]
impl NexusToolHandler for TestRunIntegrationTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("cargo", &["test", "--tests", "--quiet"], &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "stdout_tail": out.stdout.lines().rev().take(20).collect::<Vec<_>>(),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::write_subproc() }
}
