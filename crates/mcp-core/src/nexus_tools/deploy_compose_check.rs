//! `deployment::deploy_compose_check` — find docker-compose*.yml files.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeployComposeCheckTool;

#[async_trait]
impl NexusToolHandler for DeployComposeCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk_project_with(&ctx.project_root, 4, &is_skipped_dir, &mut |p, name| {
            if (name.starts_with("docker-compose") || name.starts_with("compose"))
                && (name.ends_with(".yml") || name.ends_with(".yaml"))
            {
                let content = std::fs::read_to_string(p).unwrap_or_default();
                let services = content
                    .lines()
                    .filter(|l| l.starts_with("  ") && l.trim_end().ends_with(':'))
                    .count();
                found.push(json!({"name": name, "size": content.len(), "service_lines": services}));
            }
        });
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
