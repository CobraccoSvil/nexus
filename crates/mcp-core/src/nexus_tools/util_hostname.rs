//! `utility::util_hostname` — return host name from env vars.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct UtilHostnameTool;

#[async_trait]
impl NexusToolHandler for UtilHostnameTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let host = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown".to_string());
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".to_string());
        Ok(json!({"ok": true, "hostname": host, "user": user}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
