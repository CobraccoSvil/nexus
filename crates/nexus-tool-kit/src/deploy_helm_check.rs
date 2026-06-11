//! `deployment::deploy_helm_check` — find Chart.yaml files.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeployHelmCheckTool;

#[async_trait]
impl NexusToolHandler for DeployHelmCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<String> = vec![];
        walk_project_with(&ctx.project_root, 5, &is_skipped_dir, &mut |p, name| {
            if name == "Chart.yaml" || name == "values.yaml" {
                if let Some(parent) = p.parent().and_then(|x| x.file_name()) {
                    found.push(format!("{}/{}", parent.to_string_lossy(), name));
                } else {
                    found.push(name.to_string());
                }
            }
        });
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
