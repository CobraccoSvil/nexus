//! `testing::test_ignored_count` — conta `#[ignore]` attributes.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestIgnoredCountTool;

#[async_trait]
impl NexusToolHandler for TestIgnoredCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["#[ignore]", "#[ignore ="]);
        Ok(json!({"ok": true, "files_scanned": files, "ignored": counts[0] + counts[1]}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
