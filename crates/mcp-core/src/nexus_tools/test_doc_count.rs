//! `testing::test_doc_count` — conta esempi doctest (``` blocks dentro `///`).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct TestDocCountTool;

fn count_doctests(content: &str) -> usize {
    let mut count = 0usize;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("/// ```") || trimmed.starts_with("//! ```") {
            count += 1;
        }
    }
    count / 2
}

fn walk(dir: &Path, depth: usize, total: &mut usize, files: &mut usize) {
    if depth > 8 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, total, files);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    *files += 1;
                    *total += count_doctests(&s);
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for TestDocCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut total = 0usize;
        let mut files = 0usize;
        for sub in &["src", "crates"] {
            let p = ctx.project_root.join(sub);
            if p.is_dir() { walk(&p, 0, &mut total, &mut files); }
        }
        Ok(json!({"ok": true, "files_scanned": files, "doctests": total}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
