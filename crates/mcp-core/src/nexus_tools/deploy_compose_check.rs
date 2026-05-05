//! `deployment::deploy_compose_check` — find docker-compose*.yml files.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeployComposeCheckTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<Value>) {
    if depth > 4 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if (name.starts_with("docker-compose") || name.starts_with("compose"))
                && (name.ends_with(".yml") || name.ends_with(".yaml"))
            {
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                let services = content.lines().filter(|l| l.starts_with("  ") && l.trim_end().ends_with(':')).count();
                out.push(json!({"name": name, "size": content.len(), "service_lines": services}));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DeployComposeCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
