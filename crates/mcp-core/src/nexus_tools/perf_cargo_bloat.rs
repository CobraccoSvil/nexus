//! `performance::perf_cargo_bloat` — `cargo bloat --release --crates -n 20`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfCargoBloatTool;

#[async_trait]
impl NexusToolHandler for PerfCargoBloatTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = match run_cmd(
            "cargo",
            &["bloat", "--release", "--crates", "-n", "20"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await
        {
            Ok(o) => o,
            Err(NexusToolError::BinaryMissing(_)) => {
                return Ok(
                    json!({"ok": false, "error": "cargo-bloat not installed", "hint": "cargo install cargo-bloat"}),
                );
            }
            Err(e) => return Err(e),
        };
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "stdout": out.stdout,
            "stderr_tail": out.stderr.lines().rev().take(5).collect::<Vec<_>>(),
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
