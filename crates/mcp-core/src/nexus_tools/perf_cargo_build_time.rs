//! `performance::perf_cargo_build_time` — `cargo build --timings=json` (limitato).
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfCargoBuildTimeTool;

#[async_trait]
impl NexusToolHandler for PerfCargoBuildTimeTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("cargo", &["build", "--timings", "--quiet"], &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "report_dir": "target/cargo-timings",
            "stderr_tail": out.stderr.lines().rev().take(10).collect::<Vec<_>>(),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::write_subproc() }
}
