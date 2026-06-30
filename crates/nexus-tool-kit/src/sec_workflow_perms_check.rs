//! `security::sec_workflow_perms_check` — check `.github/workflows/*.yml` for
//! `permissions:` blocks and `pull_request_target` triggers.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecWorkflowPermsCheckTool;

#[async_trait]
impl NexusToolHandler for SecWorkflowPermsCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join(".github").join("workflows");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false}));
        }
        let mut files: Vec<Value> = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !(name.ends_with(".yml") || name.ends_with(".yaml")) {
                    continue;
                }
                let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
                files.push(json!({
                    "name": name,
                    "has_permissions": content.contains("permissions:"),
                    "has_pull_request_target": content.contains("pull_request_target"),
                    "uses_secrets": content.contains("secrets."),
                }));
            }
        }
        Ok(json!({"ok": true, "exists": true, "count": files.len(), "files": files}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
