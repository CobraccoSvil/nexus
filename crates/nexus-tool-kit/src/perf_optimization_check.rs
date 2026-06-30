//! `performance::perf_optimization_check` — verifica `[profile.release]` in Cargo.toml.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfOptimizationCheckTool;

fn extract_section<'a>(toml: &'a str, header: &str) -> Option<&'a str> {
    let idx = toml.find(header)?;
    let rest = &toml[idx + header.len()..];
    let end = rest
        .find("\n[")
        .map(|p| idx + header.len() + p)
        .unwrap_or(toml.len());
    Some(&toml[idx..end])
}

fn get_kv(section: &str, key: &str) -> Option<String> {
    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(key) {
            if let Some(idx) = trimmed.find('=') {
                return Some(trimmed[idx + 1..].trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

#[async_trait]
impl NexusToolHandler for PerfOptimizationCheckTool {
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
        match extract_section(&toml, "[profile.release]") {
            Some(section) => Ok(json!({
                "ok": true,
                "exists": true,
                "opt_level": get_kv(section, "opt-level"),
                "lto": get_kv(section, "lto"),
                "codegen_units": get_kv(section, "codegen-units"),
                "strip": get_kv(section, "strip"),
                "panic": get_kv(section, "panic"),
                "debug": get_kv(section, "debug"),
            })),
            None => {
                Ok(json!({"ok": true, "exists": false, "note": "Default release profile in use"}))
            }
        }
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
