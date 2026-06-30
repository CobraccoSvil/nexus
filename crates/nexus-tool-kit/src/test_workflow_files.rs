//! `testing::test_workflow_files` — lista file YAML in `.github/workflows/` con keyword "test".
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestWorkflowFilesTool;

#[async_trait]
impl NexusToolHandler for TestWorkflowFilesTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join(".github").join("workflows");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "files": []}));
        }
        let mut files: Vec<Value> = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if !(name.ends_with(".yml") || name.ends_with(".yaml")) {
                    continue;
                }
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                let mentions_test = content.contains("test") || content.contains("cargo test");
                files.push(
                    json!({"name": name, "size": content.len(), "mentions_test": mentions_test}),
                );
            }
        }
        Ok(json!({"ok": true, "exists": true, "count": files.len(), "files": files}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
