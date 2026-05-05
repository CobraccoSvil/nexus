//! `performance::perf_lto_check` — verifica impostazione `lto` in `[profile.release]`.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfLtoCheckTool;

#[async_trait]
impl NexusToolHandler for PerfLtoCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let cargo = ctx.project_root.join("Cargo.toml");
        if !cargo.is_file() {
            return Ok(json!({"ok": false, "error": "Cargo.toml not found"}));
        }
        let toml = std::fs::read_to_string(&cargo).map_err(NexusToolError::Io)?;
        let mut in_release = false;
        let mut value: Option<String> = None;
        for line in toml.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_release = trimmed == "[profile.release]";
                continue;
            }
            if in_release && trimmed.starts_with("lto") {
                if let Some(idx) = trimmed.find('=') {
                    value = Some(trimmed[idx + 1..].trim().trim_matches('"').to_string());
                    break;
                }
            }
        }
        Ok(json!({
            "ok": true,
            "lto": value,
            "enabled": value.as_deref().map(|v| v != "false" && v != "off").unwrap_or(false),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
