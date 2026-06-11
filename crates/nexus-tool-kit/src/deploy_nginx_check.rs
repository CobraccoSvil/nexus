//! `deployment::deploy_nginx_check` — find nginx*.conf files.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeployNginxCheckTool;

#[async_trait]
impl NexusToolHandler for DeployNginxCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk_project_with(&ctx.project_root, 5, &is_skipped_dir, &mut |p, name| {
            if (name.starts_with("nginx") && name.ends_with(".conf")) || name == "nginx.conf" {
                let content = std::fs::read_to_string(p).unwrap_or_default();
                let server_blocks =
                    content.matches("server {").count() + content.matches("server{").count();
                let upstreams = content.matches("upstream ").count();
                found.push(json!({"name": name, "size": content.len(), "server_blocks": server_blocks, "upstreams": upstreams}));
            }
        });
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
