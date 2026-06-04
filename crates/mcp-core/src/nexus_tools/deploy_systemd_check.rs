//! `deployment::deploy_systemd_check` — find *.service / *.socket / *.timer units.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeploySystemdCheckTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 5 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if name.ends_with(".service")
                || name.ends_with(".socket")
                || name.ends_with(".timer")
            {
                out.push(name);
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DeploySystemdCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<String> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "units": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
