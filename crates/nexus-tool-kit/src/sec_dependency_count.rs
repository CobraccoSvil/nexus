//! `security::sec_dependency_count` — count total deps across all Cargo.toml.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct SecDependencyCountTool;

fn walk_cargo(dir: &Path, depth: usize, count: &mut usize, files: &mut usize) {
    if depth > 6 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                walk_cargo(&p, depth + 1, count, files);
            } else if name == "Cargo.toml" {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    *files += 1;
                    let mut in_deps = false;
                    for line in content.lines() {
                        let l = line.trim();
                        if l.starts_with('[') {
                            in_deps = l.contains("dependencies")
                                || l.contains("dev-dependencies")
                                || l.contains("build-dependencies");
                            continue;
                        }
                        if in_deps && !l.is_empty() && !l.starts_with('#') && l.contains('=') {
                            *count += 1;
                        }
                    }
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for SecDependencyCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut count = 0usize;
        let mut files = 0usize;
        walk_cargo(&ctx.project_root, 0, &mut count, &mut files);
        Ok(json!({"ok": true, "cargo_toml_files": files, "total_dep_lines": count}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
