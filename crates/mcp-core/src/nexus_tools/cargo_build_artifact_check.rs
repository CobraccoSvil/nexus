//! `build::cargo_build_artifact_check` — lista file in target/release.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoBuildArtifactCheckTool;

#[async_trait]
impl NexusToolHandler for CargoBuildArtifactCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let profile = args
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("release");
        let dir = ctx.project_root.join("target").join(profile);
        if !dir.exists() {
            return Ok(json!({"ok": true, "exists": false, "profile": profile}));
        }
        let mut bins = Vec::new();
        let mut total_size: u64 = 0;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                let Ok(meta) = e.metadata() else { continue };
                if meta.is_file() {
                    total_size += meta.len();
                    let name = e.file_name().to_string_lossy().into_owned();
                    // Solo file eseguibili o senza estensione (su unix)
                    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                    if ext.is_empty() || ext == "exe" {
                        bins.push(json!({"name": name, "size": meta.len()}));
                    }
                }
            }
        }
        Ok(json!({
            "ok": true,
            "exists": true,
            "profile": profile,
            "binary_count": bins.len(),
            "binaries": bins,
            "total_size_bytes": total_size,
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"profile":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
