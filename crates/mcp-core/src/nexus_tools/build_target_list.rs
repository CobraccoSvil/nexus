//! `build::build_target_list` — list subdirectories under target/.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildTargetListTool;

#[async_trait]
impl NexusToolHandler for BuildTargetListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join("target");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "subdirs": []}));
        }
        let mut subdirs: Vec<String> = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if entry.path().is_dir() {
                    subdirs.push(entry.file_name().to_string_lossy().to_string());
                }
            }
        }
        Ok(json!({"ok": true, "exists": true, "count": subdirs.len(), "subdirs": subdirs}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
