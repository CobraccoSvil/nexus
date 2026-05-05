//! `testing::test_stale_snapshots` — conta file `.snap.new` (snapshot non accettati).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct TestStaleSnapshotsTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if name.ends_with(".snap.new") {
                out.push(name);
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for TestStaleSnapshotsTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut stale: Vec<String> = vec![];
        walk(&ctx.project_root, 0, &mut stale);
        Ok(json!({"ok": true, "count": stale.len(), "files": stale}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
