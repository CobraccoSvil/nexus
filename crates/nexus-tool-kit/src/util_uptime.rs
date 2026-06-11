//! `utility::util_uptime` — process uptime since static start time.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

pub struct UtilUptimeTool;

#[async_trait]
impl NexusToolHandler for UtilUptimeTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let start = START.get_or_init(Instant::now);
        let secs = start.elapsed().as_secs();
        Ok(json!({"ok": true, "uptime_secs": secs}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
