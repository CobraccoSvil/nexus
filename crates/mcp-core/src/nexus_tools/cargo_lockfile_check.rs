//! `dependencies::cargo_lockfile_check` — verifica presenza e versione di Cargo.lock.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoLockfileCheckTool;

#[async_trait]
impl NexusToolHandler for CargoLockfileCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let lock = ctx.project_root.join("Cargo.lock");
        if !lock.exists() {
            return Ok(json!({"ok": true, "exists": false}));
        }
        let content = std::fs::read_to_string(&lock).map_err(NexusToolError::Io)?;
        let version = content.lines()
            .find(|l| l.starts_with("version = "))
            .and_then(|l| l.split('=').nth(1))
            .map(|s| s.trim().to_string());
        let package_count = content.matches("\n[[package]]").count();
        Ok(json!({
            "ok": true,
            "exists": true,
            "lockfile_version": version,
            "package_count": package_count,
            "size_bytes": content.len(),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
