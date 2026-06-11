//! `security::sec_dockerfile_user_check` — check Dockerfile USER directive.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecDockerfileUserCheckTool;

#[async_trait]
impl NexusToolHandler for SecDockerfileUserCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk_project_with(&ctx.project_root, 4, &is_skipped_dir, &mut |p, name| {
            if name == "Dockerfile" || name.starts_with("Dockerfile.") {
                if let Ok(content) = std::fs::read_to_string(p) {
                    let has_user = content.lines().any(|l| {
                        let t = l.trim();
                        t.starts_with("USER ") && !t.starts_with("USER root")
                    });
                    let runs_as_root = content
                        .lines()
                        .any(|l| l.trim() == "USER root" || l.trim() == "USER 0");
                    found.push(json!({
                        "name": name,
                        "has_non_root_user": has_user,
                        "explicit_root": runs_as_root,
                    }));
                }
            }
        });
        Ok(json!({"ok": true, "count": found.len(), "dockerfiles": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
