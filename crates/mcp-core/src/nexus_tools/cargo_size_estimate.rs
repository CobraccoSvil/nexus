//! `performance::cargo_size_estimate` — somma size dei binari in target/release.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoSizeEstimateTool;

#[async_trait]
impl NexusToolHandler for CargoSizeEstimateTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let release = ctx.project_root.join("target").join("release");
        if !release.exists() {
            return Ok(json!({"ok": true, "exists": false}));
        }
        let mut bins: Vec<(String, u64)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&release) {
            for e in entries.flatten() {
                let Ok(meta) = e.metadata() else { continue };
                if meta.is_file() {
                    let p = e.path();
                    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                    if ext.is_empty() || ext == "exe" {
                        let name = e.file_name().to_string_lossy().into_owned();
                        bins.push((name, meta.len()));
                    }
                }
            }
        }
        bins.sort_by(|a, b| b.1.cmp(&a.1));
        let total: u64 = bins.iter().map(|(_, s)| s).sum();
        let items: Vec<Value> = bins.iter().map(|(n, s)| json!({"name": n, "bytes": s, "mb": s / (1024 * 1024)})).collect();
        Ok(json!({
            "ok": true,
            "exists": true,
            "binary_count": items.len(),
            "total_bytes": total,
            "total_mb": total / (1024 * 1024),
            "binaries": items,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
