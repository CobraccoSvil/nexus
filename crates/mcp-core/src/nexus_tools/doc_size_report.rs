//! `documentation::doc_size_report` — count + bytes totali file .md nel progetto.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DocSizeReportTool;

fn walk(dir: &Path, depth: usize, count: &mut usize, bytes: &mut u64) {
    if depth > 6 {
        return;
    }
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
            walk(&p, depth + 1, count, bytes);
        } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
            *count += 1;
            if let Ok(meta) = std::fs::metadata(&p) {
                *bytes += meta.len();
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DocSizeReportTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut count = 0usize;
        let mut bytes = 0u64;
        walk(&ctx.project_root, 0, &mut count, &mut bytes);
        Ok(json!({"ok": true, "files": count, "total_bytes": bytes}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
