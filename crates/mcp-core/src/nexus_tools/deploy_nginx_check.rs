//! `deployment::deploy_nginx_check` — find nginx*.conf files.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeployNginxCheckTool;

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
            } else if (name.starts_with("nginx") && name.ends_with(".conf")) || name == "nginx.conf"
            {
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                let server_blocks =
                    content.matches("server {").count() + content.matches("server{").count();
                let upstreams = content.matches("upstream ").count();
                out.push(json!({"name": name, "size": content.len(), "server_blocks": server_blocks, "upstreams": upstreams}));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DeployNginxCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
