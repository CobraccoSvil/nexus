//! `build::cargo_run` — `cargo run --release [-- args...]`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoRunTool;

#[async_trait]
impl NexusToolHandler for CargoRunTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let release = args.get("release").and_then(Value::as_bool).unwrap_or(true);
        let mut cmd_args: Vec<&str> = vec!["run"];
        if release {
            cmd_args.push("--release");
        }
        let bin_arg;
        if let Some(b) = args.get("bin").and_then(Value::as_str) {
            bin_arg = b.to_string();
            cmd_args.push("--bin");
            cmd_args.push(&bin_arg);
        }
        let out = run_cmd("cargo", &cmd_args, &ctx.project_root, ctx.timeout_secs.max(180)).await?;
        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "stdout_preview": out.stdout.chars().take(2000).collect::<String>(),
            "stderr_preview": out.stderr.chars().take(2000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"release":{"type":"boolean"},"bin":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::write_subproc() }
}
