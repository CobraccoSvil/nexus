//! `performance::perf_box_count` — conta `Box<dyn` e `Box::new`.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfBoxCountTool;

#[async_trait]
impl NexusToolHandler for PerfBoxCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["Box<dyn ", "Box::new("]);
        Ok(json!({"ok": true, "files_scanned": files, "box_dyn": counts[0], "box_new": counts[1]}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
