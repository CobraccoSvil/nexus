//! `testing::test_count_files` — conta file *_test.rs e tests/*.rs nel progetto.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct TestCountFilesTool;

fn walk(dir: &Path, depth: usize, in_tests_dir: bool, files: &mut Vec<String>) {
    if depth > 8 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name.starts_with('.') || name == "node_modules" {
                continue;
            }
            if p.is_dir() {
                let entering_tests = name == "tests";
                walk(&p, depth + 1, in_tests_dir || entering_tests, files);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if in_tests_dir || name.ends_with("_test.rs") || name.ends_with("_tests.rs") {
                    files.push(name);
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for TestCountFilesTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut files: Vec<String> = vec![];
        walk(&ctx.project_root, 0, false, &mut files);
        Ok(
            json!({"ok": true, "count": files.len(), "samples": files.iter().take(20).collect::<Vec<_>>()}),
        )
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
