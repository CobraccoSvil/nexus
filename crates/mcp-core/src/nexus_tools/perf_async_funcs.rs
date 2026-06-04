//! `performance::perf_async_funcs` — conta `async fn` nei .rs del progetto.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfAsyncFuncsTool;

#[async_trait]
impl NexusToolHandler for PerfAsyncFuncsTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["async fn ", ".await"]);
        Ok(
            json!({"ok": true, "files_scanned": files, "async_fn": counts[0], "await_count": counts[1]}),
        )
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
