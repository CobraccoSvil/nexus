//! `build::build_debug_size` — sum binary sizes in target/debug.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildDebugSizeTool;

#[async_trait]
impl NexusToolHandler for BuildDebugSizeTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join("target").join("debug");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "total_bytes": 0}));
        }
        let mut total = 0u64;
        let mut count = 0usize;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !name.contains('.') || name.ends_with(".exe") {
                            total += meta.len();
                            count += 1;
                        }
                    }
                }
            }
        }
        Ok(json!({"ok": true, "exists": true, "total_bytes": total, "binary_count": count}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
