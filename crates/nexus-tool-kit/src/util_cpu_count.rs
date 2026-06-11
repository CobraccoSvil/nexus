//! `utility::util_cpu_count` — logical CPU count via std::thread::available_parallelism.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct UtilCpuCountTool;

#[async_trait]
impl NexusToolHandler for UtilCpuCountTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let n = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(1);
        Ok(json!({"ok": true, "logical_cpus": n}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
