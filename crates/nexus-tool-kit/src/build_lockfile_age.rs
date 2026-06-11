//! `build::build_lockfile_age` — mtime of Cargo.lock.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::SystemTime;

pub struct BuildLockfileAgeTool;

#[async_trait]
impl NexusToolHandler for BuildLockfileAgeTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let p = ctx.project_root.join("Cargo.lock");
        if !p.is_file() {
            return Ok(json!({"ok": true, "exists": false}));
        }
        let meta = std::fs::metadata(&p).map_err(NexusToolError::Io)?;
        let size = meta.len();
        let age_secs = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .map(|d| d.as_secs());
        Ok(json!({"ok": true, "exists": true, "size": size, "age_secs": age_secs}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
