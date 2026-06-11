//! `deployment::deploy_k8s_check` — find kubernetes manifests.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeployK8sCheckTool;

#[async_trait]
impl NexusToolHandler for DeployK8sCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk_project_with(&ctx.project_root, 5, &is_skipped_dir, &mut |p, name| {
            if name.ends_with(".yml") || name.ends_with(".yaml") {
                let content = std::fs::read_to_string(p).unwrap_or_default();
                if content.contains("apiVersion:") && content.contains("kind:") {
                    let kinds: Vec<&str> = content
                        .lines()
                        .filter_map(|l| l.trim().strip_prefix("kind:"))
                        .map(|s| s.trim())
                        .collect();
                    found.push(json!({"name": name, "kinds": kinds}));
                }
            }
        });
        Ok(json!({"ok": true, "count": found.len(), "manifests": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
