//! `testing::test_failed_log` — esegue `cargo test --no-run` per validare compilazione test.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestFailedLogTool;

#[async_trait]
impl NexusToolHandler for TestFailedLogTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "cargo",
            &["test", "--no-run", "--quiet"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "compile_errors": out.stderr.lines().filter(|l| l.contains("error[") || l.contains("error:")).count(),
            "stderr_tail": out.stderr.lines().rev().take(20).collect::<Vec<_>>(),
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
