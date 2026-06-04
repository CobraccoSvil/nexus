//! `performance::perf_unsafe_blocks` — conta `unsafe {` e `unsafe fn` nel progetto.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfUnsafeBlocksTool;

#[async_trait]
impl NexusToolHandler for PerfUnsafeBlocksTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["unsafe {", "unsafe fn ", "unsafe impl "],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "unsafe_blocks": counts[0],
            "unsafe_fn": counts[1],
            "unsafe_impl": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
