//! `performance::perf_codegen_units` — verifica `codegen-units` in `[profile.release]`.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfCodegenUnitsTool;

#[async_trait]
impl NexusToolHandler for PerfCodegenUnitsTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let cargo = ctx.project_root.join("Cargo.toml");
        if !cargo.is_file() {
            return Ok(json!({"ok": false, "error": "Cargo.toml not found"}));
        }
        let toml = std::fs::read_to_string(&cargo).map_err(NexusToolError::Io)?;
        let mut in_release = false;
        let mut value: Option<i64> = None;
        for line in toml.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_release = trimmed == "[profile.release]";
                continue;
            }
            if in_release && trimmed.starts_with("codegen-units") {
                if let Some(idx) = trimmed.find('=') {
                    value = trimmed[idx + 1..].trim().parse::<i64>().ok();
                    break;
                }
            }
        }
        Ok(json!({"ok": true, "codegen_units": value, "default": value.is_none()}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
