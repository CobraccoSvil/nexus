//! `build::build_release_size` — sum binary sizes in target/release.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildReleaseSizeTool;

#[async_trait]
impl NexusToolHandler for BuildReleaseSizeTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join("target").join("release");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "total_bytes": 0}));
        }
        let mut total = 0u64;
        let mut binaries: Vec<Value> = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let len = meta.len();
                        // Likely binary heuristic: no extension or .exe
                        if !name.contains('.') || name.ends_with(".exe") {
                            total += len;
                            binaries.push(json!({"name": name, "size": len}));
                        }
                    }
                }
            }
        }
        Ok(json!({"ok": true, "exists": true, "total_bytes": total, "binary_count": binaries.len(), "binaries": binaries}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
