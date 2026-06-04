//! `deployment::deploy_k8s_check` — find kubernetes manifests.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeployK8sCheckTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<Value>) {
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
            } else if name.ends_with(".yml") || name.ends_with(".yaml") {
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                if content.contains("apiVersion:") && content.contains("kind:") {
                    let kinds: Vec<&str> = content
                        .lines()
                        .filter_map(|l| l.trim().strip_prefix("kind:"))
                        .map(|s| s.trim())
                        .collect();
                    out.push(json!({"name": name, "kinds": kinds}));
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DeployK8sCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "manifests": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
