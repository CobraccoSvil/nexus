//! `performance::perf_clone_count` — conta `.clone()` e `.to_owned()`.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfCloneCountTool;

#[async_trait]
impl NexusToolHandler for PerfCloneCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &[".clone()", ".to_owned()"]);
        Ok(json!({"ok": true, "files_scanned": files, "clone": counts[0], "to_owned": counts[1]}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
