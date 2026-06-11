//! `testing::test_run_quiet` — `cargo test --quiet` su filtro opzionale.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestRunQuietTool;

#[async_trait]
impl NexusToolHandler for TestRunQuietTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let mut cmd_args: Vec<&str> = vec!["test", "--quiet"];
        let filter_val;
        if let Some(f) = args.get("filter").and_then(Value::as_str) {
            filter_val = f.to_string();
            cmd_args.push("--");
            cmd_args.push(&filter_val);
        }
        let out = run_cmd("cargo", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "stdout_tail": out.stdout.lines().rev().take(20).collect::<Vec<_>>(),
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"filter":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
