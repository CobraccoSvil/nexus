//! `deployment::deploy_ansible_check` — find ansible playbooks.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeployAnsibleCheckTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 5 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if name == "ansible.cfg" || name == "playbook.yml" || name == "hosts" || name == "inventory" {
                out.push(name);
            } else if (name.ends_with(".yml") || name.ends_with(".yaml"))
                && p.parent().and_then(|x| x.file_name()).map(|s| s.to_string_lossy().to_string()) == Some("ansible".to_string())
            {
                out.push(format!("ansible/{}", name));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DeployAnsibleCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut found: Vec<String> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
