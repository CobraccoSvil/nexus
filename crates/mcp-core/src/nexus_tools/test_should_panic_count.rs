//! `testing::test_should_panic_count` — conta `#[should_panic]` attributes.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestShouldPanicCountTool;

#[async_trait]
impl NexusToolHandler for TestShouldPanicCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["#[should_panic"]);
        Ok(json!({"ok": true, "files_scanned": files, "should_panic": counts[0]}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
