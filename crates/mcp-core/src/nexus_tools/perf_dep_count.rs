//! `performance::perf_dep_count` — conta dipendenze da Cargo.toml root.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfDepCountTool;

#[async_trait]
impl NexusToolHandler for PerfDepCountTool {
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
        let mut current = "";
        let mut deps = 0usize;
        let mut dev = 0usize;
        let mut build = 0usize;
        let mut workspace_deps = 0usize;
        for line in toml.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                current = trimmed;
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.contains('=') {
                continue;
            }
            match current {
                "[dependencies]" => deps += 1,
                "[dev-dependencies]" => dev += 1,
                "[build-dependencies]" => build += 1,
                "[workspace.dependencies]" => workspace_deps += 1,
                _ => {}
            }
        }
        Ok(
            json!({"ok": true, "dependencies": deps, "dev_dependencies": dev, "build_dependencies": build, "workspace_dependencies": workspace_deps}),
        )
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
