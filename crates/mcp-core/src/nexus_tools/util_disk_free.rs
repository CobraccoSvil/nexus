//! `utility::util_disk_free` — best-effort disk free at project root.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct UtilDiskFreeTool;

#[async_trait]
impl NexusToolHandler for UtilDiskFreeTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        // Use shell call: `df -k .` or PowerShell `Get-PSDrive` — keep portable
        // by simply reporting the cwd path metadata size info.
        let p = &ctx.project_root;
        let exists = p.exists();
        let is_dir = p.is_dir();
        let canonical = std::fs::canonicalize(p)
            .ok()
            .map(|x| x.to_string_lossy().to_string());
        Ok(json!({
            "ok": true,
            "path": p.to_string_lossy(),
            "canonical": canonical,
            "exists": exists,
            "is_dir": is_dir,
            "note": "platform-specific df not implemented; use platform tool"
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
