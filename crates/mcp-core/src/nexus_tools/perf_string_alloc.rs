//! `performance::perf_string_alloc` — conta allocazioni String comuni.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfStringAllocTool;

#[async_trait]
impl NexusToolHandler for PerfStringAllocTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["String::new(", "String::from(", "format!(", ".to_string()"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "string_new": counts[0],
            "string_from": counts[1],
            "format_macro": counts[2],
            "to_string": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
