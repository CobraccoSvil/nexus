//! `utility::util_now_iso` — current time as RFC3339 / unix epoch seconds.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct UtilNowIsoTool;

#[async_trait]
impl NexusToolHandler for UtilNowIsoTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let now = SystemTime::now();
        let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let iso = chrono::DateTime::<chrono::Utc>::from(now).to_rfc3339();
        Ok(json!({"ok": true, "epoch_secs": secs, "iso": iso}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
