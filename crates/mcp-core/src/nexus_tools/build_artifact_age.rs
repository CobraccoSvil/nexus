//! `build::build_artifact_age` — newest mtime under target/release.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::SystemTime;

pub struct BuildArtifactAgeTool;

#[async_trait]
impl NexusToolHandler for BuildArtifactAgeTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let dir = ctx.project_root.join("target").join("release");
        if !dir.is_dir() {
            return Ok(json!({"ok": true, "exists": false}));
        }
        let mut newest: Option<SystemTime> = None;
        let mut count = 0usize;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        count += 1;
                        if let Ok(m) = meta.modified() {
                            if newest.map(|n| m > n).unwrap_or(true) {
                                newest = Some(m);
                            }
                        }
                    }
                }
            }
        }
        let age_secs = newest
            .and_then(|n| SystemTime::now().duration_since(n).ok())
            .map(|d| d.as_secs());
        Ok(json!({"ok": true, "exists": true, "files": count, "newest_age_secs": age_secs}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
