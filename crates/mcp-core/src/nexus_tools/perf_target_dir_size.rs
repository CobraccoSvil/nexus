//! `performance::perf_target_dir_size` — calcola dimensione totale di target/.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct PerfTargetDirSizeTool;

fn dir_size(p: &Path, depth: usize) -> u64 {
    if depth > 8 { return 0; }
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(p) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_file() {
                total += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            } else if path.is_dir() {
                total += dir_size(&path, depth + 1);
            }
        }
    }
    total
}

#[async_trait]
impl NexusToolHandler for PerfTargetDirSizeTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let target = ctx.project_root.join("target");
        if !target.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "bytes": 0}));
        }
        let bytes = dir_size(&target, 0);
        Ok(json!({"ok": true, "exists": true, "bytes": bytes, "mb": (bytes as f64) / (1024.0 * 1024.0)}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
