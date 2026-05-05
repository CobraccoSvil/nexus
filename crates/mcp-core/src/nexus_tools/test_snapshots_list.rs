//! `testing::test_snapshots_list` — lista file `.snap` (insta).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct TestSnapshotsListTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name.starts_with('.') || name == "node_modules" { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if name.ends_with(".snap") {
                out.push(name);
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for TestSnapshotsListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut snaps: Vec<String> = vec![];
        walk(&ctx.project_root, 0, &mut snaps);
        Ok(json!({"ok": true, "count": snaps.len(), "snapshots": snaps}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
