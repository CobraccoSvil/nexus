//! `deployment::deploy_terraform_check` — find *.tf and terraform.tfstate files.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeployTerraformCheckTool;

fn walk(dir: &Path, depth: usize, tf: &mut usize, state: &mut usize) {
    if depth > 5 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name == ".terraform" || name.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, tf, state);
            } else if name.ends_with(".tf") {
                *tf += 1;
            } else if name == "terraform.tfstate" || name.ends_with(".tfstate") {
                *state += 1;
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DeployTerraformCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut tf = 0usize;
        let mut state = 0usize;
        walk(&ctx.project_root, 0, &mut tf, &mut state);
        Ok(json!({"ok": true, "tf_files": tf, "tfstate_files": state}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
