//! `documentation::doc_api_list` — lista file .md sotto docs/api.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocApiListTool;

#[async_trait]
impl NexusToolHandler for DocApiListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join("docs").join("api");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "files": []}));
        }
        let mut files: Vec<String> = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md") {
                    files.push(entry.file_name().to_string_lossy().to_string());
                }
            }
        }
        files.sort();
        Ok(json!({"ok": true, "exists": true, "count": files.len(), "files": files}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
