//! `documentation::doc_orphan_md` — file .md non referenziati da README.md.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocOrphanMdTool;

#[async_trait]
impl NexusToolHandler for DocOrphanMdTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let readme_path = ctx.project_root.join("README.md");
        let readme = std::fs::read_to_string(&readme_path).unwrap_or_default();
        let root = ctx.project_root.clone();
        let mut all_md: Vec<String> = vec![];
        walk_project_with(&ctx.project_root, 6, &is_skipped_dir, &mut |p, _name| {
            if p.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(rel) = p.strip_prefix(&root) {
                    let s = rel.to_string_lossy().replace('\\', "/");
                    if s != "README.md" {
                        all_md.push(s);
                    }
                }
            }
        });
        let orphans: Vec<String> = all_md
            .into_iter()
            .filter(|md| !readme.contains(md))
            .collect();
        Ok(json!({"ok": true, "count": orphans.len(), "orphans": orphans}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
