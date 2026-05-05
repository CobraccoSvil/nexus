//! `security::sec_dockerfile_user_check` — check Dockerfile USER directive.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct SecDockerfileUserCheckTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<Value>) {
    if depth > 4 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if name == "Dockerfile" || name.starts_with("Dockerfile.") {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    let has_user = content.lines().any(|l| {
                        let t = l.trim();
                        t.starts_with("USER ") && !t.starts_with("USER root")
                    });
                    let runs_as_root = content.lines().any(|l| l.trim() == "USER root" || l.trim() == "USER 0");
                    out.push(json!({
                        "name": name,
                        "has_non_root_user": has_user,
                        "explicit_root": runs_as_root,
                    }));
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for SecDockerfileUserCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "dockerfiles": found}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
