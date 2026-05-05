//! `utility::util_pid` — return process id of the running mcp-core.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct UtilPidTool;

#[async_trait]
impl NexusToolHandler for UtilPidTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let pid = std::process::id();
        Ok(json!({"ok": true, "pid": pid}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
