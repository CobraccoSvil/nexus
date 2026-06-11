//! `documentation::doc_size_report` — count + bytes totali file .md nel progetto.
use super::fs_scan::walk_project_with;
use super::is_skipped_dir;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocSizeReportTool;

#[async_trait]
impl NexusToolHandler for DocSizeReportTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut count = 0usize;
        let mut bytes = 0u64;
        walk_project_with(&ctx.project_root, 6, &is_skipped_dir, &mut |p, _name| {
            if p.extension().and_then(|e| e.to_str()) == Some("md") {
                count += 1;
                if let Ok(meta) = std::fs::metadata(p) {
                    bytes += meta.len();
                }
            }
        });
        Ok(json!({"ok": true, "files": count, "total_bytes": bytes}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
