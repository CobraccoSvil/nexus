//! `performance::perf_panic_count` — conta `panic!`, `unwrap()`, `expect(`.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfPanicCountTool;

#[async_trait]
impl NexusToolHandler for PerfPanicCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["panic!", ".unwrap()", ".expect(", "todo!", "unimplemented!"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "panic": counts[0],
            "unwrap": counts[1],
            "expect": counts[2],
            "todo": counts[3],
            "unimplemented": counts[4],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
