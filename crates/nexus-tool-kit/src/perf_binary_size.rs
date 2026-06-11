//! `performance::perf_binary_size` — dimensione dei binari in target/release.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfBinarySizeTool;

#[async_trait]
impl NexusToolHandler for PerfBinarySizeTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join("target").join("release");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false, "binaries": []}));
        }
        let mut bins: Vec<Value> = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let is_bin = !name.contains('.') || name.ends_with(".exe");
                if !is_bin {
                    continue;
                }
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                bins.push(json!({"name": name, "bytes": size}));
            }
        }
        bins.sort_by(|a, b| {
            b["bytes"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["bytes"].as_u64().unwrap_or(0))
        });
        Ok(json!({"ok": true, "exists": true, "count": bins.len(), "binaries": bins}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
