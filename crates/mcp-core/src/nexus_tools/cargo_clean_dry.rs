//! `build::cargo_clean_dry` — calcola dimensione di target/ senza rimuoverla.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct CargoCleanDryTool;

fn dir_size(p: &Path, depth: usize) -> u64 {
    if depth > 12 {
        return 0;
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(p) {
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                total += dir_size(&e.path(), depth + 1);
            } else {
                total += meta.len();
            }
        }
    }
    total
}

#[async_trait]
impl NexusToolHandler for CargoCleanDryTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let target = ctx.project_root.join("target");
        if !target.exists() {
            return Ok(json!({"ok": true, "exists": false, "size_bytes": 0}));
        }
        let size = dir_size(&target, 0);
        Ok(json!({
            "ok": true,
            "exists": true,
            "target_dir": target.to_string_lossy(),
            "size_bytes": size,
            "size_mb": size / (1024 * 1024),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
