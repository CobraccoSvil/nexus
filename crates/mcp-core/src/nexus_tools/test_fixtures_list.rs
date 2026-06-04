//! `testing::test_fixtures_list` — lista contenuto `tests/fixtures/`.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestFixturesListTool;

#[async_trait]
impl NexusToolHandler for TestFixturesListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join("tests").join("fixtures");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "files": []}));
        }
        let mut files: Vec<Value> = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let size = std::fs::metadata(entry.path())
                    .map(|m| m.len())
                    .unwrap_or(0);
                files.push(json!({"name": name, "size": size}));
            }
        }
        Ok(json!({"ok": true, "exists": true, "count": files.len(), "files": files}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
