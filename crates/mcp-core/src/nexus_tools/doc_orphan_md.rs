//! `documentation::doc_orphan_md` — file .md non referenziati da README.md.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct DocOrphanMdTool;

fn collect_md(dir: &Path, root: &Path, out: &mut Vec<String>, depth: usize) {
    if depth > 6 { return; }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            collect_md(&p, root, out, depth + 1);
        } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(rel) = p.strip_prefix(root) {
                let s = rel.to_string_lossy().replace('\\', "/");
                if s != "README.md" {
                    out.push(s);
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DocOrphanMdTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let readme_path = ctx.project_root.join("README.md");
        let readme = std::fs::read_to_string(&readme_path).unwrap_or_default();
        let mut all_md: Vec<String> = vec![];
        let root: PathBuf = ctx.project_root.clone();
        collect_md(&root, &root, &mut all_md, 0);
        let orphans: Vec<String> = all_md.into_iter()
            .filter(|md| !readme.contains(md))
            .collect();
        Ok(json!({"ok": true, "count": orphans.len(), "orphans": orphans}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
