//! `deployment::deploy_ansible_check` — find ansible playbooks.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeployAnsibleCheckTool;

#[async_trait]
impl NexusToolHandler for DeployAnsibleCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<String> = vec![];
        walk_project_with(&ctx.project_root, 5, &is_skipped_dir, &mut |p, name| {
            if name == "ansible.cfg" || name == "playbook.yml" || name == "hosts" || name == "inventory"
            {
                found.push(name.to_string());
            } else if (name.ends_with(".yml") || name.ends_with(".yaml"))
                && p.parent()
                    .and_then(|x| x.file_name())
                    .map(|s| s.to_string_lossy().to_string())
                    == Some("ansible".to_string())
            {
                found.push(format!("ansible/{}", name));
            }
        });
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
